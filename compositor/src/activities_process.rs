//! Explicitly enabled fallback. Bounded /proc sampling and regex work stay on
//! one worker; disappearance emits owner exit, never an inferred exit status.
use crate::ipc::IpcEvent;
use ferese_activities::Owner;
use smithay::reexports::calloop::channel;
use std::collections::BTreeMap;
use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub(crate) struct Watcher(Arc<AtomicBool>);
impl Drop for Watcher {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
impl Watcher {
    pub fn start(
        rules: crate::activities::SharedRules,
        sender: channel::SyncSender<IpcEvent>,
    ) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        std::thread::Builder::new()
            .name("ferese-activity-processes".into())
            .spawn(move || {
                let mut observed = BTreeMap::<
                    Owner,
                    (
                        Arc<std::os::fd::OwnedFd>,
                        ferese_config::activities::Matched,
                    ),
                >::new();
                while !worker_stop.load(Ordering::Acquire) {
                    let rules = rules.read().expect("activity rules").clone();
                    let dead: Vec<_> = observed
                        .iter()
                        .filter_map(|(owner, (fd, _))| {
                            crate::ipc::activity_owner::exited(fd).then_some(*owner)
                        })
                        .collect();
                    for owner in dead {
                        observed.remove(&owner);
                        if sender.send(IpcEvent::ActivityOwnerExited(owner)).is_err() {
                            return;
                        }
                    }
                    // Reconcile every detection: admission may have failed or its lease expired.
                    for (owner, (fd, matched)) in &observed {
                        let _ = sender.try_send(IpcEvent::ActivityDetected(
                            *owner,
                            Box::new(matched.clone()),
                            fd.clone(),
                        ));
                    }
                    if let Ok(processes) = std::fs::read_dir("/proc") {
                        for process in processes.take(4_096).flatten() {
                            if worker_stop.load(Ordering::Acquire) || observed.len() >= 128 {
                                break;
                            }
                            let Some(pid) = process
                                .file_name()
                                .to_str()
                                .and_then(|name| name.parse::<u32>().ok())
                            else {
                                continue;
                            };
                            let Ok((owner, fd, parents)) = crate::ipc::activity_owner::inspect(pid)
                            else {
                                continue;
                            };
                            if observed.contains_key(&owner)
                                || parents.iter().any(|parent| parent == "feresectl")
                            {
                                continue;
                            }
                            let Ok(file) = std::fs::File::open(process.path().join("cmdline"))
                            else {
                                continue;
                            };
                            let mut bytes = Vec::new();
                            if file.take(4_097).read_to_end(&mut bytes).is_err()
                                || bytes.len() > 4_096
                            {
                                continue;
                            }
                            let Ok(command) = String::from_utf8(bytes) else {
                                continue;
                            };
                            let command = command.trim_end_matches('\0').replace('\0', " ");
                            let Ok(cwd) = std::fs::read_link(process.path().join("cwd")) else {
                                continue;
                            };
                            let cwd = cwd.to_string_lossy();
                            let Some(matched) = rules.matcher.matches(&command, &cwd, &parents)
                            else {
                                continue;
                            };
                            let fd = Arc::new(fd);
                            // Keep the inspected process identity alive through dispatch.
                            // No raw command, cwd or environment crosses the dispatcher.
                            if sender
                                .try_send(IpcEvent::ActivityDetected(
                                    owner,
                                    Box::new(matched.clone()),
                                    fd.clone(),
                                ))
                                .is_ok()
                            {
                                observed.insert(owner, (fd, matched));
                            }
                        }
                    }
                    std::thread::sleep(Duration::from_secs(2));
                }
            })?;
        Ok(Self(stop))
    }
}

//! Keep the compositor responsive without promoting its workers or clients.
use std::io;
use std::time::Duration;

use zbus::blocking::{Connection, Proxy};

const SERVICE: &str = "org.freedesktop.RealtimeKit1";
const PATH: &str = "/org/freedesktop/RealtimeKit1";
const NICE: i32 = -5;

pub(crate) fn init() {
    // Respect an explicitly configured scheduling policy or nice level.
    // SAFETY: both queries operate on the calling thread and take no pointers.
    let (policy, nice) = unsafe {
        (
            libc::sched_getscheduler(0),
            libc::getpriority(libc::PRIO_PROCESS, 0),
        )
    };
    if policy & !libc::SCHED_RESET_ON_FORK != libc::SCHED_OTHER || nice != 0 {
        tracing::debug!(policy, nice, "keeping configured compositor priority");
        return;
    }
    if let Err(error) = reset_children() {
        tracing::warn!(%error, "could not isolate compositor priority; keeping normal priority");
        return;
    }

    // SAFETY: gettid has no pointer arguments and identifies this event-loop thread.
    let tid = unsafe { libc::gettid() } as u64;
    // D-Bus activation and policy checks never block frame dispatch. The new
    // thread starts at normal priority, even if the request completes quickly.
    if let Err(error) = std::thread::Builder::new()
        .name("ferese-priority".into())
        .spawn(move || {
            let result = zbus::blocking::connection::Builder::system()
                .and_then(|builder| builder.method_timeout(Duration::from_secs(1)).build())
                .and_then(|connection| request(&connection, tid));
            match result {
                Ok(nice) => tracing::info!(tid, nice, "compositor scheduling priority selected"),
                Err(error) => tracing::warn!(%error, "could not confirm compositor priority"),
            }
        })
    {
        tracing::warn!(%error, "could not start compositor priority request");
    }
}

fn reset_children() -> io::Result<()> {
    let parameter = libc::sched_param { sched_priority: 0 };
    // Linux resets negative nice values on both fork and thread creation. This
    // avoids passing the compositor's priority to compilers, decoders or apps.
    // SAFETY: parameter is initialized and valid for this call; tid 0 is this thread.
    if unsafe {
        libc::sched_setscheduler(0, libc::SCHED_OTHER | libc::SCHED_RESET_ON_FORK, &parameter)
    } == -1
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn request(connection: &Connection, tid: u64) -> zbus::Result<i32> {
    // RTKit exposes Properties.Get but not GetAll. This one-shot query must
    // not trigger zbus's automatic property-cache population.
    let proxy = zbus::blocking::proxy::Builder::<Proxy<'_>>::new(connection)
        .destination(SERVICE)?
        .path(PATH)?
        .interface(SERVICE)?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()?;
    let minimum: i32 = proxy.get_property("MinNiceLevel")?;
    let nice = NICE.max(minimum).min(0);
    if nice < 0 {
        let _: () = proxy.call("MakeThreadHighPriority", &(tid, nice))?;
    }
    Ok(nice)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
    use std::sync::{Arc, Mutex};

    struct Bus(std::process::Child);
    impl Drop for Bus {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    struct Rtkit {
        minimum: Arc<AtomicI32>,
        reject: Arc<AtomicBool>,
        calls: Arc<Mutex<Vec<(u64, i32)>>>,
    }

    #[zbus::interface(name = "org.freedesktop.RealtimeKit1")]
    impl Rtkit {
        #[zbus(property)]
        fn min_nice_level(&self) -> i32 {
            self.minimum.load(Ordering::Relaxed)
        }

        fn make_thread_high_priority(&self, tid: u64, nice: i32) -> zbus::fdo::Result<()> {
            if self.reject.load(Ordering::Relaxed) {
                return Err(zbus::fdo::Error::AccessDenied("priority denied".into()));
            }
            self.calls.lock().unwrap().push((tid, nice));
            Ok(())
        }
    }

    #[test]
    fn request_targets_the_event_loop_respects_policy_and_handles_denial_and_disconnect() {
        let mut bus = Bus(std::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap());
        let mut address = String::new();
        std::io::BufReader::new(bus.0.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let minimum = Arc::new(AtomicI32::new(-15));
        let reject = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let service = zbus::blocking::connection::Builder::address(address.trim())
            .unwrap()
            .serve_at(
                PATH,
                Rtkit {
                    minimum: minimum.clone(),
                    reject: reject.clone(),
                    calls: calls.clone(),
                },
            )
            .unwrap()
            .name(SERVICE)
            .unwrap()
            .build()
            .unwrap();
        let client = zbus::blocking::connection::Builder::address(address.trim())
            .unwrap()
            .method_timeout(Duration::from_millis(200))
            .build()
            .unwrap();
        let target = 123_456_u64;
        assert_eq!(request(&client, target).unwrap(), -5);
        minimum.store(-3, Ordering::Relaxed);
        assert_eq!(request(&client, target).unwrap(), -3);
        minimum.store(0, Ordering::Relaxed);
        assert_eq!(request(&client, target).unwrap(), 0);
        assert_eq!(*calls.lock().unwrap(), [(target, -5), (target, -3)]);
        minimum.store(-15, Ordering::Relaxed);
        reject.store(true, Ordering::Relaxed);
        assert!(request(&client, target).is_err());
        assert_eq!(calls.lock().unwrap().len(), 2);
        drop(service);
        assert!(request(&client, target).is_err());
    }

    #[test]
    #[ignore = "requires the host RTKit service to grant negative nice priority"]
    fn priority_is_limited_to_the_compositor_thread_and_resets_for_workers_and_clients() {
        const TEST: &str = "priority::tests::priority_is_limited_to_the_compositor_thread_and_resets_for_workers_and_clients";
        if !crate::startup_tests::private_runtime(TEST) {
            return;
        }
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .try_init();
        init();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let nice = loop {
            // SAFETY: getpriority with tid 0 queries the calling thread.
            let nice = unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) };
            if nice < 0 {
                break nice;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "RTKit must allow a priority boost for this test"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        // SAFETY: getpriority and sched_getscheduler query the calling thread.
        assert_eq!(unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) }, nice);
        // SAFETY: sched_getscheduler has no pointer arguments.
        let policy = unsafe { libc::sched_getscheduler(0) };
        assert_eq!(policy, libc::SCHED_RESET_ON_FORK);
        let worker = std::thread::spawn(|| {
            // SAFETY: these scalar calls query the new worker's own scheduling state.
            unsafe {
                (
                    libc::getpriority(libc::PRIO_PROCESS, 0),
                    libc::sched_getscheduler(0),
                )
            }
        })
        .join()
        .unwrap();
        assert_eq!(worker, (0, libc::SCHED_OTHER));
        let child = crate::process::command("python3")
            .args([
                "-c",
                "import os; print(os.getpriority(os.PRIO_PROCESS, 0), os.sched_getscheduler(0))",
            ])
            .output()
            .unwrap();
        assert!(child.status.success());
        assert_eq!(String::from_utf8(child.stdout).unwrap().trim(), "0 0");
    }

    #[test]
    #[ignore = "manual CPU-frame benchmark; requires the host RTKit service"]
    fn profile_frame_dispatch_under_cpu_contention() {
        const TEST: &str = "priority::tests::profile_frame_dispatch_under_cpu_contention";
        const CHILD: &str = "FERESE_CPU_FRAME_BENCHMARK_CHILD";
        if std::env::var_os(CHILD).is_none() {
            // Isolate affinity and priority changes while preserving benchmark
            // output; this test creates no compositor sockets or desktop state.
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", TEST, "--ignored", "--nocapture"])
                .env(CHILD, "1")
                .output()
                .unwrap();
            eprint!("{}", String::from_utf8_lossy(&output.stderr));
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
            return;
        }
        reset_children().unwrap();
        // SAFETY: zero is valid storage for cpu_set_t before sched_getaffinity fills it.
        let mut allowed: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        // SAFETY: allowed is writable for its full size, and tid 0 is this thread.
        let status =
            unsafe { libc::sched_getaffinity(0, std::mem::size_of_val(&allowed), &mut allowed) };
        assert_eq!(status, 0);
        let cpu = (0..libc::CPU_SETSIZE as usize)
            .rev()
            .find(|cpu| {
                // SAFETY: cpu is within cpu_set_t and allowed is initialized.
                unsafe { libc::CPU_ISSET(*cpu, &allowed) }
            })
            .unwrap();
        // Confine this short experiment to one CPU, leaving the other CPUs
        // available to the real desktop. New workers inherit this affinity.
        // SAFETY: cpu is an allowed index and the initialized set lives through the call.
        unsafe {
            libc::CPU_ZERO(&mut allowed);
            libc::CPU_SET(cpu, &mut allowed);
            assert_eq!(
                libc::sched_setaffinity(0, std::mem::size_of_val(&allowed), &allowed),
                0
            );
        }
        struct Stop<'a>(&'a AtomicBool);
        impl Drop for Stop<'_> {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Relaxed);
            }
        }
        fn cpu_time() -> Duration {
            let mut time = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            // SAFETY: time is initialized and writable; this clock measures this thread.
            let status = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut time) };
            assert_eq!(status, 0);
            Duration::new(time.tv_sec as u64, time.tv_nsec as u32)
        }
        fn measure(label: &str, cpu: usize, run: u32) {
            let interval = Duration::from_nanos(1_000_000_000 / 120);
            let epoch = std::time::Instant::now();
            let mut scheduler = crate::frame_scheduler::FrameScheduler::new(interval);
            scheduler.presented(Duration::ZERO);
            let mut wakeups = Vec::new();
            let mut work = Vec::new();
            let mut previous = Duration::ZERO;
            let mut missed_slots = 0;
            // Simulate CPU submission work and observed refresh timestamps;
            // this does not measure GPU completion or physical display frames.
            for _ in 0..120 {
                let now = epoch.elapsed();
                scheduler.request(now);
                let plan = scheduler.plan(now).unwrap();
                std::thread::sleep(plan.render_at.saturating_sub(epoch.elapsed()));
                let dispatch = epoch.elapsed();
                scheduler.begin(dispatch);
                wakeups.push(dispatch.saturating_sub(plan.render_at));
                let started = cpu_time();
                while cpu_time().saturating_sub(started) < Duration::from_millis(2) {
                    for _ in 0..64 {
                        std::hint::spin_loop();
                    }
                }
                let finished = epoch.elapsed();
                let cost = finished.saturating_sub(dispatch);
                work.push(cost);
                scheduler.rendered(plan, finished, cost, true);
                let slot = finished.as_nanos().div_ceil(interval.as_nanos()) as u32;
                let presented = interval * slot;
                missed_slots += (presented.saturating_sub(previous).as_nanos()
                    / interval.as_nanos())
                .saturating_sub(1);
                std::thread::sleep(presented.saturating_sub(epoch.elapsed()));
                scheduler.presented(presented);
                previous = presented;
            }
            wakeups.sort_unstable();
            work.sort_unstable();
            eprintln!(
                "CPU frame simulation: {label} run={run} cpu={cpu} frames=120 target_misses={} missed_refresh_slots={missed_slots} wake_p99_us={} work_p99_us={} elapsed_ms={}",
                scheduler.missed_deadlines,
                wakeups[118].as_micros(),
                work[118].as_micros(),
                epoch.elapsed().as_millis()
            );
        }
        let stop = AtomicBool::new(false);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    while !stop.load(Ordering::Relaxed) {
                        std::hint::spin_loop();
                    }
                });
            }
            let _stop = Stop(&stop);
            std::thread::sleep(Duration::from_millis(100));
            // SAFETY: this query identifies the benchmark's live timer thread.
            let tid = unsafe { libc::gettid() } as u64;
            let connection = zbus::blocking::connection::Builder::system()
                .unwrap()
                .method_timeout(Duration::from_secs(1))
                .build()
                .unwrap();
            for run in 1..=3 {
                measure("normal", cpu, run);
                assert_eq!(request(&connection, tid).unwrap(), NICE);
                measure("nice=-5", cpu, run);
                // SAFETY: increasing this thread's nice value back to zero
                // requires no privilege and does not change another thread.
                assert_eq!(unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, 0) }, 0);
            }
        });
    }
}

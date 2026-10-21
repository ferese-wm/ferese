#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub(super) struct Entry {
    pub keys: String,
    pub description: String,
}

pub(super) fn load() -> Result<Vec<Entry>, String> {
    let entries: Vec<Entry> =
        serde_json::from_value(crate::compositor_ipc::call("get-keybindings")?)
            .map_err(|error| error.to_string())?;
    if entries.len() > 256
        || entries
            .iter()
            .any(|entry| entry.keys.len() > 256 || entry.description.len() > 512)
    {
        return Err("Invalid keybinding guide".into());
    }
    Ok(entries)
}

impl crate::FereseShell {
    pub(super) fn load_guide(
        &mut self,
        manual: bool,
        output: Option<String>,
    ) -> cosmic::app::Task<crate::Message> {
        let generation = self.guide_load.begin(manual);
        cosmic::app::Task::perform(
            async {
                tokio::task::spawn_blocking(load)
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|result| result)
            },
            move |result| {
                cosmic::Action::App(crate::Message::GuideLoaded {
                    generation,
                    manual,
                    output: output.clone(),
                    result,
                })
            },
        )
    }

    pub(super) fn toggle_guide(&mut self, output: String) -> cosmic::app::Task<crate::Message> {
        if let Some(modal) = &mut self.system_modal {
            if !modal.is_guide() {
                return cosmic::app::Task::none();
            }
            if modal.motion.closing() {
                modal.motion.retarget(1.0, std::time::Instant::now());
                return cosmic::app::Task::none();
            }
            return self.close_system_modal();
        }
        // A second press before the asynchronous fetch completes cancels it.
        // Its eventual result cannot reopen the dialog or finish a newer load.
        if self.guide_load.loading {
            self.guide_load.cancel();
            self.guide_shown = true;
            return cosmic::app::Task::none();
        }
        if self.pending_power.is_some() || self.outputs.is_empty() {
            return cosmic::app::Task::none();
        }
        self.guide_shown = true;
        self.load_guide(true, Some(output))
    }
}

#[derive(Default)]
pub(super) struct LoadState {
    pub(super) loading: bool,
    pub(super) manual: bool,
    generation: u64,
}

impl LoadState {
    fn begin(&mut self, manual: bool) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.loading = true;
        self.manual = manual;
        self.generation
    }

    fn cancel(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.loading = false;
    }

    pub(super) fn finish(&mut self, generation: u64) -> bool {
        if generation != self.generation {
            return false;
        }
        self.loading = false;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelling_and_reopening_rejects_delayed_loads() {
        let mut state = LoadState::default();
        let initial = state.begin(false);
        state.cancel();
        assert!(!state.finish(initial));
        assert!(!state.loading);
        let manual = state.begin(true);
        assert!(!state.finish(initial));
        assert!(state.loading && state.manual);
        assert!(state.finish(manual));
        assert!(!state.loading && state.manual);
    }
}

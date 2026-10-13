use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Status {
    pub enabled: bool,
    pub temperature: u16,
    pub supported_outputs: usize,
    pub active_outputs: usize,
    pub failed_outputs: usize,
}

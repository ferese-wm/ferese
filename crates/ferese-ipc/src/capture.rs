use serde::{Deserialize, Serialize};

pub const SERVICE: &str = "org.freedesktop.impl.portal.desktop.ferese";
pub const PATH: &str = "/org/ferese/CaptureStatus";
pub const INTERFACE: &str = "org.ferese.CaptureStatus";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub app: String,
    pub sources: Vec<String>,
}

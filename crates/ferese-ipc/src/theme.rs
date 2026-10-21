use ferese_theme_model::{Mode, ResolvedTheme, families::Family};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::Connection;

pub const SCHEMA_VERSION: u32 = 2;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Snapshot {
    pub version: u32,
    pub revision: u64,
    pub mode: Mode,
    pub theme: ResolvedTheme,
    pub presented: ResolvedTheme,
    pub warnings: Vec<String>,
    pub error: Option<String>,
    pub families: Vec<Family>,
    #[serde(default)]
    pub fallback_note: Option<String>,
}

impl Snapshot {
    /// Decode older messages with the caller's catalog. Explicit empty catalogs stay empty.
    pub fn decode(
        mut value: Value,
        fallback_families: impl FnOnce() -> Vec<Family>,
    ) -> Result<Self, String> {
        if let Some(object) = value.as_object_mut()
            && !object.contains_key("families")
        {
            object.insert(
                "families".into(),
                serde_json::to_value(fallback_families()).map_err(|e| e.to_string())?,
            );
        }
        let snapshot: Self = serde_json::from_value(value).map_err(|e| e.to_string())?;
        if snapshot.version != SCHEMA_VERSION {
            return Err("Unsupported theme snapshot version".into());
        }
        Ok(snapshot)
    }
}

impl Connection {
    pub fn get(
        &mut self,
        fallback_families: impl FnOnce() -> Vec<Family>,
    ) -> Result<Snapshot, String> {
        Snapshot::decode(self.call("theme-get", json!({}))?, fallback_families)
    }

    pub fn watch(
        &mut self,
        revision: u64,
        fallback_families: impl FnOnce() -> Vec<Family>,
    ) -> Result<Snapshot, String> {
        Snapshot::decode(
            self.wait("theme-watch", json!({"since": revision}))?,
            fallback_families,
        )
    }
}

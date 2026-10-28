use std::sync::Arc;

use smithay::reexports::wayland_server::Client;
use smithay::wayland::security_context::{
    SecurityContext, SecurityContextHandler, SecurityContextListenerSource,
};

use crate::Ferese;
use crate::state::ClientState;

pub(crate) fn unrestricted_client(client: &Client) -> bool {
    client
        .get_data::<ClientState>()
        .is_some_and(|state| state.security_context.is_none())
}

impl SecurityContextHandler for Ferese {
    fn context_created(&mut self, source: SecurityContextListenerSource, context: SecurityContext) {
        if let Err(error) = self
            .loop_handle
            .insert_source(source, move |stream, _, state| {
                let data = ClientState {
                    security_context: Some(context.clone()),
                    ..Default::default()
                };
                if let Err(error) = state.display_handle.insert_client(stream, Arc::new(data)) {
                    tracing::warn!(%error, "failed to register sandboxed Wayland client");
                }
            })
        {
            tracing::warn!(%error, "failed to register security context listener");
        }
    }
}

#[cfg(test)]
mod tests;

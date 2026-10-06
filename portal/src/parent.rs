pub(crate) type Attachment = Result<
    (
        Option<std::sync::Arc<Parent>>,
        Result<ferese_theme_client::material::ModalMaterial, String>,
    ),
    String,
>;

use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_registry, wl_surface};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, delegate_noop};
use wayland_protocols::xdg::foreign::zv2::client::zxdg_imported_v2;
use wayland_protocols::xdg::foreign::zv2::client::zxdg_imported_v2::ZxdgImportedV2;
use wayland_protocols::xdg::foreign::zv2::client::zxdg_importer_v2::ZxdgImporterV2;

/// Longest parent identifier accepted before it is treated as hostile input.
const MAX_PARENT_IDENTIFIER: usize = 4096;

/// Outcome of resolving a portal parent window.
#[derive(Debug, Default)]
pub(crate) struct ParentAttachment {
    /// The imported parent, when one could be established.
    pub parent: Option<Parent>,
    /// Set when parenting is unavailable but the request may still proceed.
    ///
    /// The dialog is shown unparented. This is deliberately *not* an error:
    /// failing the whole file chooser or consent prompt because a caller used an
    /// X11 parent would deny an otherwise valid operation.
    pub diagnostic: Option<String>,
}

/// Classify a portal parent identifier without contacting the compositor.
///
/// A valid `x11:` identifier is not an error condition: Ferese does not yet
/// have a verified XID-to-exported-Wayland-parent bridge, so the request
/// degrades to an unparented window. Crucially, an `x11:` identifier is never
/// reinterpreted as a Wayland foreign handle.
fn classify_parent<'a>(parent: &'a str) -> ParentKind<'a> {
    if parent.is_empty() {
        return ParentKind::None;
    }
    if parent.len() > MAX_PARENT_IDENTIFIER {
        return ParentKind::Unsupported;
    }
    if let Some(handle) = parent.strip_prefix("wayland:") {
        return if handle.is_empty() {
            ParentKind::Unsupported
        } else {
            ParentKind::Wayland(handle)
        };
    }
    if let Some(handle) = parent.strip_prefix("x11:") {
        return if is_x11_window_identifier(handle) {
            ParentKind::X11
        } else {
            // A malformed XID is a protocol error, not an X11 parent to
            // degrade from: accepting arbitrary text here would let any string
            // be reported as an X11 window.
            ParentKind::Unsupported
        };
    }
    ParentKind::Unsupported
}

/// Whether `identifier` is a syntactically valid X11 window identifier.
///
/// XIDs are unsigned 32-bit hex, optionally with the `0x` prefix, as X11
/// clients write them (for example `0x2400003` or `1a2b3c`).
fn is_x11_window_identifier(identifier: &str) -> bool {
    let digits = identifier
        .strip_prefix("0x")
        .or_else(|| identifier.strip_prefix("0X"))
        .unwrap_or(identifier);

    !digits.is_empty() && digits.len() <= 8 && digits.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Debug, PartialEq, Eq)]
enum ParentKind<'a> {
    /// No parent requested.
    None,
    /// A Wayland foreign-toplevel handle that can be imported.
    Wayland(&'a str),
    /// A valid but unsupported X11 parent window identifier.
    X11,
    /// Malformed or unrecognized; this stays a hard error.
    Unsupported,
}

#[derive(Debug)]
pub(crate) struct Parent {
    connection: Connection,
    imported: ZxdgImportedV2,
    importer: ZxdgImporterV2,
}

impl Parent {
    pub(crate) fn attach(window: &dyn cosmic::iced::window::Window, parent: &str) -> Result<ParentAttachment, String> {
        use cosmic::iced::window::raw_window_handle::{RawDisplayHandle, RawWindowHandle};
        let handle = match classify_parent(parent) {
            ParentKind::None => {
                return Ok(ParentAttachment::default());
            }
            ParentKind::X11 => {
                // Degrade, do not fail. Authorization and consent checks are
                // unaffected; only the transient parent relationship is lost.
                // No parent is invented, and the untrusted XID is not used to
                // identify a process or to grant any permission.
                return Ok(ParentAttachment {
                    parent: None,
                    diagnostic: Some(
                        "showing this dialog unparented: X11 parent windows are not supported yet".to_owned(),
                    ),
                });
            }
            ParentKind::Unsupported => {
                return Err("Unsupported portal parent window identifier".into());
            }
            ParentKind::Wayland(handle) => handle,
        };
        let display = window.display_handle().map_err(|error| error.to_string())?;
        let surface = window.window_handle().map_err(|error| error.to_string())?;
        let (RawDisplayHandle::Wayland(display), RawWindowHandle::Wayland(surface)) =
            (display.as_raw(), surface.as_raw())
        else {
            return Err("Portal parenting requires Wayland".into());
        };
        // The window lends live handles; the borrowed backend never owns the display.
        let backend =
            unsafe { wayland_client::backend::Backend::from_foreign_display(display.display.as_ptr().cast()) };
        let connection = Connection::from_backend(backend);
        let id = unsafe {
            wayland_client::backend::ObjectId::from_ptr(
                wl_surface::WlSurface::interface(),
                surface.surface.as_ptr().cast(),
            )
        }
        .map_err(|error| error.to_string())?;
        let surface = wl_surface::WlSurface::from_id(&connection, id).map_err(|error| error.to_string())?;
        let (globals, mut queue) = registry_queue_init::<State>(&connection).map_err(|error| error.to_string())?;
        let qh = queue.handle();
        let importer = globals
            .bind::<ZxdgImporterV2, _, _>(&qh, 1..=1, ())
            .map_err(|error| error.to_string())?;
        let imported = importer.import_toplevel(handle.into(), &qh, ());
        let mut state = State::default();
        queue.roundtrip(&mut state).map_err(|error| error.to_string())?;
        if state.invalid {
            imported.destroy();
            importer.destroy();
            let _ = connection.flush();
            return Err("Portal parent window no longer exists".into());
        }
        imported.set_parent_of(&surface);
        connection.flush().map_err(|error| error.to_string())?;
        Ok(ParentAttachment {
            parent: Some(Self {
                connection,
                imported,
                importer,
            }),
            diagnostic: None,
        })
    }
}

impl Drop for Parent {
    fn drop(&mut self) {
        self.imported.destroy();
        self.importer.destroy();
        let _ = self.connection.flush();
    }
}

#[derive(Default)]
struct State {
    invalid: bool,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZxdgImportedV2, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZxdgImportedV2,
        event: zxdg_imported_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, zxdg_imported_v2::Event::Destroyed) {
            state.invalid = true;
        }
    }
}

delegate_noop!(State: ignore ZxdgImporterV2);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_x11_parent_degrades_instead_of_failing_the_request() {
        // The whole point: a valid X11 parent must not fail an otherwise valid
        // file chooser or consent prompt.
        assert!(matches!(classify_parent("x11:0x2400003"), ParentKind::X11));
        assert!(matches!(classify_parent("x11:1a2b3c"), ParentKind::X11));
    }

    #[test]
    fn an_x11_parent_is_never_reinterpreted_as_a_wayland_handle() {
        // A Wayland handle is the raw remainder after the prefix. Treating an
        // XID as one would ask the compositor to import a nonsense handle and,
        // worse, could match some unrelated exported surface.
        match classify_parent("x11:0x2400003") {
            ParentKind::Wayland(handle) => panic!("an X11 parent must not become a Wayland handle: {handle}"),
            ParentKind::X11 => {}
            other => panic!("unexpected classification: {other:?}"),
        }
    }

    #[test]
    fn wayland_parents_are_still_imported() {
        assert!(matches!(
            classify_parent("wayland:handle-1"),
            ParentKind::Wayland("handle-1")
        ));
        assert_eq!(classify_parent(""), ParentKind::None);
    }

    #[test]
    fn malformed_x11_window_identifiers_are_errors_not_parents() {
        // Anything that is not an X11 window identifier must never be reported
        // as one: doing so would turn arbitrary caller text into a window.
        for parent in [
            "x11:",
            "x11:not-a-window",
            "x11:0xzzzz",
            "x11:0x2400003extra",
            "x11:9999999999",
            "x11:-1",
        ] {
            assert!(
                matches!(classify_parent(parent), ParentKind::Unsupported),
                "{parent} must stay a hard error"
            );
        }

        // Every form an X11 client actually produces is still accepted.
        for parent in ["x11:0x2400003", "x11:1a2b3c", "x11:0X1", "x11:ffffffff"] {
            assert!(
                matches!(classify_parent(parent), ParentKind::X11),
                "{parent} is a valid window identifier"
            );
        }
    }

    #[test]
    fn malformed_identifiers_remain_hard_errors() {
        // An empty remainder, an unknown scheme, and an absurdly long value all
        // stay errors rather than silently degrading.
        for parent in ["x11:", "wayland:", "dbus:", "not-a-parent", "X11:0x1"] {
            assert!(
                matches!(classify_parent(parent), ParentKind::Unsupported),
                "{parent} must stay a hard error"
            );
        }
        let huge = format!("wayland:{}", "a".repeat(MAX_PARENT_IDENTIFIER + 1));
        assert!(matches!(classify_parent(&huge), ParentKind::Unsupported));
    }
}

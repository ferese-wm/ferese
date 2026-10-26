use crate::Ferese;
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, protocol as wl,
};
use smithay::reexports::{
    wayland_protocols as protocols, wayland_protocols_misc as misc, wayland_protocols_wlr as wlr,
};
use smithay::wayland::{Dispatch2, GlobalDispatch2};

// Forward stock protocol interfaces through Smithay's current dispatch API.
// Keep xdg_toplevel and ext_session_lock_v1 explicit: Ferese validates their
// requests before delegation, including ownership of the active session lock.
macro_rules! delegate_protocols {
    ($($interface:path),* $(,)?) => {
        $(
            impl<U: Dispatch2<$interface, Ferese>> Dispatch<$interface, U> for Ferese {
                fn request(
                    state: &mut Self,
                    client: &Client,
                    resource: &$interface,
                    request: <$interface as Resource>::Request,
                    data: &U,
                    display: &DisplayHandle,
                    init: &mut DataInit<'_, Self>,
                ) {
                    data.request(state, client, resource, request, display, init);
                }

                fn destroyed(state: &mut Self, client: ClientId, resource: &$interface, data: &U) {
                    data.destroyed(state, client, resource);
                }
            }

            impl<U: GlobalDispatch2<$interface, Ferese>> GlobalDispatch<$interface, U> for Ferese {
                fn bind(
                    state: &mut Self,
                    display: &DisplayHandle,
                    client: &Client,
                    resource: New<$interface>,
                    data: &U,
                    init: &mut DataInit<'_, Self>,
                ) {
                    data.bind(state, display, client, resource, init);
                }

                fn can_view(client: Client, data: &U) -> bool {
                    data.can_view(&client)
                }
            }
        )*
    };
}

delegate_protocols!(
    protocols::ext::foreign_toplevel_list::v1::server::ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1,
    protocols::ext::foreign_toplevel_list::v1::server::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    protocols::ext::image_capture_source::v1::server::ext_image_capture_source_v1::ExtImageCaptureSourceV1,
    protocols::ext::image_capture_source::v1::server::ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1,
    protocols::ext::image_copy_capture::v1::server::ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1,
    protocols::ext::image_copy_capture::v1::server::ext_image_copy_capture_session_v1::ExtImageCopyCaptureSessionV1,
    protocols::ext::image_copy_capture::v1::server::ext_image_copy_capture_cursor_session_v1::ExtImageCopyCaptureCursorSessionV1,
    protocols::ext::image_copy_capture::v1::server::ext_image_copy_capture_frame_v1::ExtImageCopyCaptureFrameV1,
    protocols::wp::idle_inhibit::zv1::server::zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1,
    protocols::wp::idle_inhibit::zv1::server::zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1,
    protocols::ext::data_control::v1::server::ext_data_control_device_v1::ExtDataControlDeviceV1,
    protocols::ext::data_control::v1::server::ext_data_control_manager_v1::ExtDataControlManagerV1,
    protocols::ext::data_control::v1::server::ext_data_control_source_v1::ExtDataControlSourceV1,
    protocols::ext::idle_notify::v1::server::ext_idle_notification_v1::ExtIdleNotificationV1,
    protocols::ext::idle_notify::v1::server::ext_idle_notifier_v1::ExtIdleNotifierV1,
    protocols::ext::session_lock::v1::server::ext_session_lock_manager_v1::ExtSessionLockManagerV1,
    protocols::ext::session_lock::v1::server::ext_session_lock_surface_v1::ExtSessionLockSurfaceV1,
    protocols::wp::alpha_modifier::v1::server::wp_alpha_modifier_surface_v1::WpAlphaModifierSurfaceV1,
    protocols::wp::alpha_modifier::v1::server::wp_alpha_modifier_v1::WpAlphaModifierV1,
    protocols::wp::cursor_shape::v1::server::wp_cursor_shape_device_v1::WpCursorShapeDeviceV1,
    protocols::wp::cursor_shape::v1::server::wp_cursor_shape_manager_v1::WpCursorShapeManagerV1,
    protocols::wp::fractional_scale::v1::server::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    protocols::wp::fractional_scale::v1::server::wp_fractional_scale_v1::WpFractionalScaleV1,
    protocols::wp::keyboard_shortcuts_inhibit::zv1::server::zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1,
    protocols::wp::keyboard_shortcuts_inhibit::zv1::server::zwp_keyboard_shortcuts_inhibitor_v1::ZwpKeyboardShortcutsInhibitorV1,
    protocols::wp::linux_dmabuf::zv1::server::zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1,
    protocols::wp::linux_dmabuf::zv1::server::zwp_linux_dmabuf_feedback_v1::ZwpLinuxDmabufFeedbackV1,
    protocols::wp::linux_dmabuf::zv1::server::zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1,
    protocols::wp::pointer_constraints::zv1::server::zwp_confined_pointer_v1::ZwpConfinedPointerV1,
    protocols::wp::pointer_constraints::zv1::server::zwp_locked_pointer_v1::ZwpLockedPointerV1,
    protocols::wp::pointer_constraints::zv1::server::zwp_pointer_constraints_v1::ZwpPointerConstraintsV1,
    protocols::wp::pointer_gestures::zv1::server::zwp_pointer_gestures_v1::ZwpPointerGesturesV1,
    protocols::wp::pointer_gestures::zv1::server::zwp_pointer_gesture_swipe_v1::ZwpPointerGestureSwipeV1,
    protocols::wp::pointer_gestures::zv1::server::zwp_pointer_gesture_pinch_v1::ZwpPointerGesturePinchV1,
    protocols::wp::pointer_gestures::zv1::server::zwp_pointer_gesture_hold_v1::ZwpPointerGestureHoldV1,
    protocols::wp::presentation_time::server::wp_presentation::WpPresentation,
    protocols::wp::presentation_time::server::wp_presentation_feedback::WpPresentationFeedback,
    protocols::wp::primary_selection::zv1::server::zwp_primary_selection_device_manager_v1::ZwpPrimarySelectionDeviceManagerV1,
    protocols::wp::primary_selection::zv1::server::zwp_primary_selection_device_v1::ZwpPrimarySelectionDeviceV1,
    protocols::wp::primary_selection::zv1::server::zwp_primary_selection_source_v1::ZwpPrimarySelectionSourceV1,
    protocols::wp::relative_pointer::zv1::server::zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1,
    protocols::wp::relative_pointer::zv1::server::zwp_relative_pointer_v1::ZwpRelativePointerV1,
    protocols::wp::security_context::v1::server::wp_security_context_manager_v1::WpSecurityContextManagerV1,
    protocols::wp::security_context::v1::server::wp_security_context_v1::WpSecurityContextV1,
    protocols::wp::single_pixel_buffer::v1::server::wp_single_pixel_buffer_manager_v1::WpSinglePixelBufferManagerV1,
    protocols::wp::text_input::zv3::server::zwp_text_input_manager_v3::ZwpTextInputManagerV3,
    protocols::wp::text_input::zv3::server::zwp_text_input_v3::ZwpTextInputV3,
    protocols::wp::viewporter::server::wp_viewport::WpViewport,
    protocols::wp::viewporter::server::wp_viewporter::WpViewporter,
    protocols::xdg::activation::v1::server::xdg_activation_token_v1::XdgActivationTokenV1,
    protocols::xdg::activation::v1::server::xdg_activation_v1::XdgActivationV1,
    protocols::xdg::decoration::zv1::server::zxdg_decoration_manager_v1::ZxdgDecorationManagerV1,
    protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::ZxdgToplevelDecorationV1,
    protocols::xdg::foreign::zv2::server::zxdg_exported_v2::ZxdgExportedV2,
    protocols::xdg::foreign::zv2::server::zxdg_exporter_v2::ZxdgExporterV2,
    protocols::xdg::foreign::zv2::server::zxdg_imported_v2::ZxdgImportedV2,
    protocols::xdg::foreign::zv2::server::zxdg_importer_v2::ZxdgImporterV2,
    protocols::xdg::shell::server::xdg_popup::XdgPopup,
    protocols::xdg::shell::server::xdg_positioner::XdgPositioner,
    protocols::xdg::shell::server::xdg_surface::XdgSurface,
    protocols::xdg::shell::server::xdg_wm_base::XdgWmBase,
    protocols::xdg::toplevel_icon::v1::server::xdg_toplevel_icon_manager_v1::XdgToplevelIconManagerV1,
    protocols::xdg::toplevel_icon::v1::server::xdg_toplevel_icon_v1::XdgToplevelIconV1,
    protocols::xdg::xdg_output::zv1::server::zxdg_output_manager_v1::ZxdgOutputManagerV1,
    protocols::xdg::xdg_output::zv1::server::zxdg_output_v1::ZxdgOutputV1,
    misc::zwp_input_method_v2::server::zwp_input_method_keyboard_grab_v2::ZwpInputMethodKeyboardGrabV2,
    misc::zwp_input_method_v2::server::zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
    misc::zwp_input_method_v2::server::zwp_input_method_v2::ZwpInputMethodV2,
    misc::zwp_input_method_v2::server::zwp_input_popup_surface_v2::ZwpInputPopupSurfaceV2,
    wlr::data_control::v1::server::zwlr_data_control_device_v1::ZwlrDataControlDeviceV1,
    wlr::data_control::v1::server::zwlr_data_control_manager_v1::ZwlrDataControlManagerV1,
    wlr::data_control::v1::server::zwlr_data_control_source_v1::ZwlrDataControlSourceV1,
    wlr::layer_shell::v1::server::zwlr_layer_shell_v1::ZwlrLayerShellV1,
    wlr::layer_shell::v1::server::zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
    wl::wl_buffer::WlBuffer,
    wl::wl_callback::WlCallback,
    wl::wl_compositor::WlCompositor,
    wl::wl_data_device::WlDataDevice,
    wl::wl_data_device_manager::WlDataDeviceManager,
    wl::wl_data_source::WlDataSource,
    wl::wl_keyboard::WlKeyboard,
    wl::wl_output::WlOutput,
    wl::wl_pointer::WlPointer,
    wl::wl_region::WlRegion,
    wl::wl_seat::WlSeat,
    wl::wl_shm::WlShm,
    wl::wl_shm_pool::WlShmPool,
    wl::wl_subcompositor::WlSubcompositor,
    wl::wl_subsurface::WlSubsurface,
    wl::wl_surface::WlSurface,
    wl::wl_touch::WlTouch,
);

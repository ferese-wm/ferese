use std::fs::File;

use smithay::output::Output;
use smithay::reexports::wayland_protocols_wlr::gamma_control::v1::server::{
    zwlr_gamma_control_manager_v1::{self, ZwlrGammaControlManagerV1},
    zwlr_gamma_control_v1::{self, ZwlrGammaControlV1},
};
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};

use crate::Ferese;
use crate::backends::direct::gamma;
use crate::gamma::Ramp;

pub(crate) fn init_global(display: &DisplayHandle) {
    display.create_global::<Ferese, ZwlrGammaControlManagerV1, ()>(1, ());
}

impl GlobalDispatch<ZwlrGammaControlManagerV1, ()> for Ferese {
    fn can_view(client: Client, _data: &()) -> bool {
        super::security_context::unrestricted_client(&client)
    }

    fn bind(
        _state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrGammaControlManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZwlrGammaControlManagerV1, ()> for Ferese {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &ZwlrGammaControlManagerV1,
        request: zwlr_gamma_control_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwlr_gamma_control_manager_v1::Request::GetGammaControl { id, output } => {
                let output = Output::from_resource(&output);
                let control = data_init.init(id, output.clone());
                if let Some(size) = output
                    .as_ref()
                    .and_then(|output| gamma::acquire(state, output, &control))
                {
                    control.gamma_size(size as u32);
                } else {
                    control.failed();
                }
            }
            zwlr_gamma_control_manager_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }
}

impl Dispatch<ZwlrGammaControlV1, Option<Output>> for Ferese {
    fn request(
        state: &mut Self,
        _client: &Client,
        control: &ZwlrGammaControlV1,
        request: zwlr_gamma_control_v1::Request,
        output: &Option<Output>,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwlr_gamma_control_v1::Request::SetGamma { fd } => {
                let Some(output) = output else { return };
                let Some(size) = gamma::size(state, output, control) else {
                    return;
                };
                match Ramp::read(File::from(fd), size) {
                    Ok(ramp) => gamma::set(state, output, control, ramp),
                    Err(error) => control.post_error(
                        zwlr_gamma_control_v1::Error::InvalidGamma,
                        format!("Invalid gamma table: {error}"),
                    ),
                }
            }
            zwlr_gamma_control_v1::Request::Destroy => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: smithay::reexports::wayland_server::backend::ClientId,
        control: &ZwlrGammaControlV1,
        output: &Option<Output>,
    ) {
        if let Some(output) = output {
            gamma::release(state, output, control);
        }
    }
}

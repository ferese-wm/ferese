use cosmic::iced::advanced::{Layout, Shell, clipboard, layout, mouse, widget};
use cosmic::iced::{Event as IcedEvent, Font, Pixels, Point, Size};

use super::*;
use crate::status::wireless::Security;
use crate::status_ui::{MenuStyle, PopoverAnchor};

fn wifi(path: &str, name: &str) -> WifiNetwork {
    WifiNetwork {
        path: path.into(),
        device: "/device".into(),
        ssid: name.as_bytes().to_vec(),
        name: name.into(),
        strength: 80,
        connected: false,
        security: Security::Personal,
        saved: None,
    }
}

fn open_wifi(app: &mut FereseShell) {
    app.status_service = crate::status::Service::disconnected_for_test();
    app.status.network = Some(crate::status::Network {
        enabled: true,
        ..Default::default()
    });
    let bar = app.outputs[0].bar;
    drop(app.open_menu(
        Menu::Network,
        PopoverAnchor {
            parent: bar,
            panel: app.config.panels[0].id.clone(),
            item: None,
            rectangle: cosmic::iced::Rectangle {
                x: 0,
                y: 0,
                width: 24,
                height: 26,
            },
        },
    ));
}

#[test]
fn closing_a_password_prompt_clears_its_owned_secret() {
    let mut app = crate::tests::shell_with_measured_panel();
    open_wifi(&mut app);
    drop(app.update_connections(Event::Wifi(wifi("/ap/a", "Home"))));
    drop(app.update_connections(Event::Password(Password::new("dummy-password".into()))));
    assert_eq!(app.connections.password.as_str(), "dummy-password");
    drop(app.close_menu());
    assert!(app.connections.wifi.is_none());
    assert!(app.connections.password.as_str().is_empty());
}

#[test]
fn closing_another_output_preserves_owned_wifi_and_bluetooth_input() {
    for morph in [false, true] {
        for bluetooth in [false, true] {
            let mut app = crate::tests::shell_with_measured_panel();
            open_wifi(&mut app);
            let mut second = crate::tests::shell_with_measured_panel();
            let other = second.outputs.pop().unwrap();
            let other_bar = other.bar;
            app.outputs.push(other);
            if morph {
                let mut config = app.config.clone();
                config.panels[0].style = ferese_panel::PanelStyle::Morph;
                drop(app.apply_config(config));
            }
            app.status.bluetooth = Some(crate::status::Bluetooth {
                enabled: true,
                ..Default::default()
            });
            let kind = if bluetooth {
                Menu::Bluetooth
            } else {
                Menu::Network
            };
            if morph {
                drop(app.open_hub(app.outputs[0].bar, kind));
            } else if bluetooth {
                let anchor = app.menu.as_ref().unwrap().anchor.clone();
                drop(app.open_menu(kind, anchor));
            }
            let owner = app.connection_menu().unwrap().0;
            if bluetooth {
                drop(app.update_connections(Event::Bluetooth(BluetoothDevice {
                    path: "/device/selected".into(),
                    name: "Test".into(),
                    paired: false,
                    connected: false,
                })));
                let (request, _response) = pairing::test_request(1, pairing::Prompt::Pin);
                app.connections.pairing = Some(request);
                app.connections.pairing_text = Password::new("1234".into());
            } else {
                drop(app.update_connections(Event::Wifi(wifi("/ap/home", "Home"))));
                drop(
                    app.update_connections(Event::Password(Password::new("test-password".into()))),
                );
            }
            drop(app.suspend_hub(other_bar));
            if morph {
                drop(app.destroy_menu());
            }
            if bluetooth {
                assert!(
                    app.connections.pairing.is_some(),
                    "another output cleared pairing"
                );
                assert!(!app.connections.pending.as_ref().unwrap().cancelled);
                assert_eq!(app.connections.pairing_owner, Some(owner));
            } else {
                assert_eq!(app.connections.password.as_str(), "test-password");
                assert!(app.connections.wifi.is_some());
                assert_eq!(app.connections.owner, Some(owner));
            }
            if morph {
                drop(app.suspend_hub(owner));
            } else {
                drop(app.close_menu());
            }
            assert!(app.connections.wifi.is_none());
            assert!(app.connections.password.as_str().is_empty());
            assert!(app.connections.pairing.is_none());
            if bluetooth {
                assert!(app.connections.pending.as_ref().unwrap().cancelled);
            }
        }
    }
}

#[test]
fn old_connection_and_pairing_completions_do_not_replace_current_intent() {
    let mut app = crate::tests::shell_with_measured_panel();
    let old = app.begin_connection(Discovery::Wifi, "First".into(), "/ap/first".into());
    let current = app.begin_connection(
        Discovery::Bluetooth,
        "Second".into(),
        "/device/second".into(),
    );
    drop(app.update_connections(Event::Finished(old, Err("old failure".into()))));
    assert_eq!(app.connections.pending.as_ref().unwrap().serial, current);
    assert!(app.connections.error.is_none());
    let (request, mut response) = pairing::test_request(2, pairing::Prompt::Confirm(Some(123456)));
    app.connections.pairing = Some(request);
    drop(app.update_connections(Event::PairingReply(current, 1, true)));
    assert!(response.try_recv().unwrap().is_none());
    assert!(app.connections.pairing.is_some());
    drop(app.update_connections(Event::PairingReply(current, 2, true)));
    assert!(response.try_recv().unwrap().is_some());
}

#[test]
fn bluetooth_cleanup_failure_remains_visible_after_cancellation() {
    let mut app = crate::tests::shell_with_measured_panel();
    open_wifi(&mut app);
    app.status.bluetooth = Some(crate::status::Bluetooth {
        enabled: true,
        ..Default::default()
    });
    let anchor = app.menu.as_ref().unwrap().anchor.clone();
    drop(app.open_menu(Menu::Bluetooth, anchor));
    drop(app.update_connections(Event::Bluetooth(BluetoothDevice {
        path: "/device/selected".into(),
        name: "Test".into(),
        paired: false,
        connected: false,
    })));
    let pending = app.connections.pending.as_mut().unwrap();
    pending.cancelled = true;
    let serial = pending.serial;
    drop(app.update_connections(Event::CancellationFailed(
        serial,
        "Could not disconnect the device".into(),
    )));
    assert!(app.connections.pending.is_none());
    assert_eq!(
        app.connections.error,
        Some((
            Discovery::Bluetooth,
            "Could not disconnect the device".into()
        ))
    );
}

#[test]
fn a_network_reorder_during_a_press_never_activates_the_replacement_row() {
    use cosmic::iced::advanced::renderer::Headless;
    let renderer = cosmic::iced::futures::executor::block_on(<cosmic::Renderer as Headless>::new(
        Font::default(),
        Pixels(13.),
        Some("tiny-skia"),
    ))
    .unwrap();
    let mut app = crate::tests::shell_with_measured_panel();
    open_wifi(&mut app);
    app.status.network.as_mut().unwrap().available =
        Some(vec![wifi("/ap/a", "First"), wifi("/ap/b", "Second")]);
    let style = MenuStyle {
        theme: app.config.theme,
        primary: crate::color(app.config.theme.text_primary),
        muted: crate::color(app.config.theme.text_muted),
        opacity: 1.,
    };
    let mut view: cosmic::Element<'_, cosmic::Action<Message>> =
        crate::status_ui::network::view(&app, cosmic::widget::column::with_capacity(4), style)
            .into();
    let mut tree = widget::Tree::new(&view);
    let node = view.as_widget_mut().layout(
        &mut tree,
        &renderer,
        &layout::Limits::new(Size::ZERO, Size::new(280., 600.)),
    );
    let point = Point::new(30., 15.);
    let mut messages = Vec::new();
    for event in [
        IcedEvent::Mouse(mouse::Event::CursorMoved { position: point }),
        IcedEvent::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
    ] {
        view.as_widget_mut().update(
            &mut tree,
            &event,
            Layout::new(&node),
            mouse::Cursor::Available(point),
            &renderer,
            &mut clipboard::Null,
            &mut Shell::new(&mut messages),
            &node.bounds(),
        );
    }
    assert!(messages.is_empty());
    drop(view);
    app.status
        .network
        .as_mut()
        .unwrap()
        .available
        .as_mut()
        .unwrap()
        .reverse();
    let mut view: cosmic::Element<'_, cosmic::Action<Message>> =
        crate::status_ui::network::view(&app, cosmic::widget::column::with_capacity(4), style)
            .into();
    tree.diff(&mut view);
    let node = view.as_widget_mut().layout(
        &mut tree,
        &renderer,
        &layout::Limits::new(Size::ZERO, Size::new(280., 600.)),
    );
    view.as_widget_mut().update(
        &mut tree,
        &IcedEvent::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
        Layout::new(&node),
        mouse::Cursor::Available(point),
        &renderer,
        &mut clipboard::Null,
        &mut Shell::new(&mut messages),
        &node.bounds(),
    );
    assert!(
        messages.is_empty(),
        "replacing the row under a press must not connect another network: {messages:?}"
    );
    for event in [
        IcedEvent::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
        IcedEvent::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
    ] {
        view.as_widget_mut().update(
            &mut tree,
            &event,
            Layout::new(&node),
            mouse::Cursor::Available(point),
            &renderer,
            &mut clipboard::Null,
            &mut Shell::new(&mut messages),
            &node.bounds(),
        );
    }
    let selected = messages
        .iter()
        .filter_map(|message| match message {
            cosmic::Action::App(Message::Connections(Event::Wifi(network))) => {
                Some(network.path.as_str())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        selected,
        ["/ap/b"],
        "a fresh click must still activate the row under the pointer"
    );
}

#[test]
fn scrolling_reaches_the_sixth_network_and_nearby_device_after_five_paired_devices() {
    use cosmic::iced::advanced::renderer::Headless;
    let renderer = cosmic::iced::futures::executor::block_on(<cosmic::Renderer as Headless>::new(
        Font::default(),
        Pixels(13.),
        Some("tiny-skia"),
    ))
    .unwrap();
    for bluetooth in [false, true] {
        let mut app = crate::tests::shell_with_measured_panel();
        open_wifi(&mut app);
        app.status.network.as_mut().unwrap().available = Some(
            (0..6)
                .map(|n| wifi(&format!("/ap/n{n}"), &format!("Network {n}")))
                .collect(),
        );
        app.status.bluetooth = Some(crate::status::Bluetooth {
            enabled: true,
            available: Some(
                (0..6)
                    .map(|n| BluetoothDevice {
                        path: format!("/device/n{n}"),
                        name: format!("Device {n}"),
                        paired: n < 5,
                        connected: false,
                    })
                    .collect(),
            ),
            ..Default::default()
        });
        let style = MenuStyle {
            theme: app.config.theme,
            primary: crate::color(app.config.theme.text_primary),
            muted: crate::color(app.config.theme.text_muted),
            opacity: 1.,
        };
        let rows = cosmic::widget::column::with_capacity(4);
        let mut view: cosmic::Element<'_, cosmic::Action<Message>> = if bluetooth {
            crate::status_ui::bluetooth::view(&app, rows, style).into()
        } else {
            crate::status_ui::network::view(&app, rows, style).into()
        };
        let mut tree = widget::Tree::new(&view);
        let node = view.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(280., 720.)),
        );
        let point = Point::new(30., if bluetooth { 210. } else { 126. });
        let mut messages = Vec::new();
        for event in [
            IcedEvent::Mouse(mouse::Event::CursorMoved { position: point }),
            IcedEvent::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Lines { x: 0., y: -100. },
            }),
            IcedEvent::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            IcedEvent::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
        ] {
            view.as_widget_mut().update(
                &mut tree,
                &event,
                Layout::new(&node),
                mouse::Cursor::Available(point),
                &renderer,
                &mut clipboard::Null,
                &mut Shell::new(&mut messages),
                &node.bounds(),
            );
        }
        let targets: Vec<_> = messages
            .iter()
            .filter_map(|message| match message {
                cosmic::Action::App(Message::Connections(Event::Wifi(network))) => {
                    Some(network.path.as_str())
                }
                cosmic::Action::App(Message::Connections(Event::Bluetooth(device))) => {
                    Some(device.path.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            targets,
            [if bluetooth { "/device/n5" } else { "/ap/n5" }],
            "the sixth entry must remain selectable"
        );
    }
}

#[test]
fn incompatible_saved_profiles_do_not_reopen_a_password_prompt() {
    let mut app = crate::tests::shell_with_measured_panel();
    open_wifi(&mut app);
    let mut network = wifi("/ap/a", "Home");
    network.saved = Some("/saved/home".into());
    app.status.network.as_mut().unwrap().available = Some(vec![network.clone()]);
    drop(app.update_connections(Event::Wifi(network)));
    let serial = app.connections.pending.as_ref().unwrap().serial;
    drop(app.update_connections(Event::WifiFinished(
        serial,
        Err("The profile is no longer compatible".into()),
    )));
    assert!(app.connections.wifi.is_none());
    assert!(app.connections.failed_wifi.is_none());
    assert!(app.connections.error.is_some());
}

#[test]
fn connection_dialogs_keep_one_header_and_bound_list_height() {
    use cosmic::iced::Color;
    use cosmic::iced::advanced::{Renderer, renderer::Headless};
    let mut renderer = cosmic::iced::futures::executor::block_on(
        <cosmic::Renderer as Headless>::new(Font::default(), Pixels(13.), Some("tiny-skia")),
    )
    .unwrap();
    for light in [false, true] {
        for (kind, long) in [
            (Menu::Network, false),
            (Menu::Bluetooth, false),
            (Menu::Battery, false),
            (Menu::Network, true),
            (Menu::Bluetooth, true),
        ] {
            let mut app = crate::tests::shell_with_measured_panel();
            app.status_service = crate::status::Service::disconnected_for_test();
            let mut resolved = ferese_config::theme::default_theme();
            if light {
                resolved.appearance = ferese_config::theme::Appearance::Light;
                resolved.tokens =
                    ferese_config::theme::preset("ferese-blue-light", resolved.appearance).unwrap();
            }
            app.config.apply_theme(&resolved);
            app.render = crate::rendering::Rendering::new(&app.config);
            let mut home = wifi("/ap/home", "Home");
            home.connected = true;
            home.saved = Some("/saved/home".into());
            let mut guest = wifi("/ap/guest", "Ferese Guest");
            guest.security = Security::Open;
            let available = if long {
                (0..20)
                    .map(|number| wifi(&format!("/ap/n{number}"), &format!("Network {number}")))
                    .collect()
            } else {
                vec![home, guest, wifi("/ap/studio", "Studio")]
            };
            app.status = crate::status::Snapshot {
                network: Some(crate::status::Network {
                    enabled: true,
                    connection: Some("Home".into()),
                    signal: 80,
                    available: Some(available),
                    ..Default::default()
                }),
                bluetooth: Some(crate::status::Bluetooth {
                    enabled: true,
                    devices: vec!["Headphones".into()],
                    available: Some(if long {
                        (0..20)
                            .map(|number| BluetoothDevice {
                                path: format!("/device/n{number}"),
                                name: format!("Device {number}"),
                                paired: false,
                                connected: false,
                            })
                            .collect()
                    } else {
                        vec![
                            BluetoothDevice {
                                path: "/device/headphones".into(),
                                name: "Headphones".into(),
                                paired: true,
                                connected: true,
                            },
                            BluetoothDevice {
                                path: "/device/keyboard".into(),
                                name: "Keyboard".into(),
                                paired: true,
                                connected: false,
                            },
                            BluetoothDevice {
                                path: "/device/speaker".into(),
                                name: "Speaker".into(),
                                paired: false,
                                connected: false,
                            },
                        ]
                    }),
                }),
                battery: Some(crate::status::Battery {
                    percent: 92,
                    status: "Charging".into(),
                }),
                power_profiles: Some(crate::status::PowerProfiles {
                    available: [true; 3],
                    active: "balanced".into(),
                }),
                poweroff: true,
                reboot: true,
                suspend: true,
                ..Default::default()
            };
            let bar = app.outputs[0].bar;
            drop(app.open_menu(
                kind,
                PopoverAnchor {
                    parent: bar,
                    panel: app.config.panels[0].id.clone(),
                    item: None,
                    rectangle: Default::default(),
                },
            ));
            let now = std::time::Instant::now();
            app.menu
                .as_mut()
                .unwrap()
                .motion
                .begin(now - std::time::Duration::from_secs(1));
            let mut view = app.view_status_menu_at(now);
            let mut tree = widget::Tree::new(&view);
            let node = view.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, Size::new(kind.width(), 720.)),
            );
            let bounds = node.bounds();
            assert_eq!(bounds.width, kind.width());
            assert!(bounds.height <= 380., "{kind:?} grew to {}", bounds.height);
            let a11y =
                view.as_widget()
                    .a11y_nodes(Layout::new(&node), &tree, mouse::Cursor::Unavailable);
            let nodes = a11y.root().iter().chain(a11y.children());
            let headers = nodes
                .filter(|node| {
                    node.node().label() == Some(kind.title())
                        || node.node().value() == Some(kind.title())
                })
                .count();
            assert_eq!(headers, 1, "{kind:?} should have exactly one heading");
            for scale in [1., 1.25, 1.5] {
                renderer.reset(bounds);
                view.as_widget().draw(
                    &tree,
                    &mut renderer,
                    &app.config.theme.palette().native_theme(),
                    &cosmic::iced::advanced::renderer::Style {
                        text_color: crate::color(app.config.theme.text_primary),
                        icon_color: crate::color(app.config.theme.text_primary),
                        scale_factor: f64::from(scale),
                    },
                    Layout::new(&node),
                    mouse::Cursor::Unavailable,
                    &bounds,
                );
                let size = Size::new(
                    (bounds.width * scale).ceil() as u32,
                    (bounds.height * scale).ceil() as u32,
                );
                let pixels = Headless::screenshot(&mut renderer, size, scale, Color::TRANSPARENT);
                if let Ok(directory) = std::env::var("FERESE_CONNECTION_CAPTURE_DIR") {
                    std::fs::create_dir_all(&directory).unwrap();
                    image_codec::save_buffer(
                        std::path::Path::new(&directory)
                            .join(format!("{kind:?}-light{light}-long{long}-{scale}.png")),
                        &pixels,
                        size.width,
                        size.height,
                        image_codec::ColorType::Rgba8,
                    )
                    .unwrap();
                }
            }
        }
    }
}

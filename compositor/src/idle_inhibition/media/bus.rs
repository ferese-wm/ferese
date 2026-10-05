use std::collections::HashMap;
use std::time::Duration;

use calloop::channel::SyncSender;
use ferese_ipc::{
    Response,
    media::{Playback, now_us},
};
use futures_lite::{StreamExt, future, stream};
use zbus::zvariant::{Array, OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, MatchRule, MessageStream};

use super::policy::PlayersState;
use super::{Command, Player, Update};

const PREFIX: &str = "org.mpris.MediaPlayer2";
const PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const MAX_PLAYERS: usize = 64;
type Properties = HashMap<String, OwnedValue>;

async fn bus_call<T: serde::de::DeserializeOwned + zbus::zvariant::Type>(
    connection: &Connection,
    member: &str,
    name: &str,
) -> zbus::Result<T> {
    connection
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            member,
            &(name,),
        )
        .await?
        .body()
        .deserialize()
}

async fn properties(connection: &Connection, owner: &str, interface: &str) -> zbus::Result<Properties> {
    connection
        .call_method(Some(owner), PATH, Some(PROPERTIES), "GetAll", &(interface,))
        .await?
        .body()
        .deserialize()
}

fn string<'a>(properties: &'a Properties, name: &str) -> Option<&'a str> {
    properties.get(name).and_then(|value| <&str>::try_from(value).ok())
}

pub(super) fn clean(value: &str, length: usize) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(length)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn boolean(properties: &Properties, name: &str) -> bool {
    properties
        .get(name)
        .and_then(|value| bool::try_from(value).ok())
        .unwrap_or(false)
}

fn integer(properties: &Properties, name: &str) -> Option<u64> {
    properties
        .get(name)
        .and_then(|value| i64::try_from(value).ok())
        .and_then(|value| value.try_into().ok())
}

fn number(properties: &Properties, name: &str) -> Option<f64> {
    properties
        .get(name)
        .and_then(|value| f64::try_from(value).ok())
        .filter(|value| value.is_finite())
}

async fn refresh(connection: &Connection, player: &mut Player, identity: bool) -> zbus::Result<()> {
    let mut fields = properties(connection, &player.owner, PLAYER).await?;
    let view = &mut player.view;
    view.status = match string(&fields, "PlaybackStatus") {
        Some("Playing") => Playback::Playing,
        Some("Paused") => Playback::Paused,
        _ => Playback::Stopped,
    };
    player.playing = view.status == Playback::Playing;
    view.position_us = integer(&fields, "Position");
    view.sampled_at_us = now_us();
    view.rate = number(&fields, "Rate")
        .filter(|rate| *rate != 0.0 && rate.abs() <= 100.0)
        .unwrap_or(1.0);
    view.volume = number(&fields, "Volume").filter(|volume| *volume >= 0.0);
    view.can_control = boolean(&fields, "CanControl");
    view.can_play = boolean(&fields, "CanPlay");
    view.can_pause = boolean(&fields, "CanPause");
    view.can_next = boolean(&fields, "CanGoNext");
    view.can_previous = boolean(&fields, "CanGoPrevious");
    view.can_seek = boolean(&fields, "CanSeek");
    let metadata = fields
        .remove("Metadata")
        .and_then(|value| Properties::try_from(value).ok())
        .unwrap_or_default();
    view.title = clean(string(&metadata, "xesam:title").unwrap_or_default(), 256);
    view.album = clean(string(&metadata, "xesam:album").unwrap_or_default(), 256);
    view.artist = metadata
        .get("xesam:artist")
        .and_then(|value| <&Array>::try_from(value).ok())
        .map(|array| {
            array
                .inner()
                .iter()
                .take(8)
                .filter_map(|value| <&str>::try_from(value).ok())
                .map(|artist| clean(artist, 64))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    view.length_us = integer(&metadata, "mpris:length").filter(|length| *length > 0);
    view.track_id = metadata
        .get("mpris:trackid")
        .and_then(|value| <&zbus::zvariant::ObjectPath>::try_from(value).ok())
        .map(|path| path.to_string());
    view.art_url = string(&metadata, "mpris:artUrl")
        .filter(|url| !url.is_empty() && url.len() <= 256 * 1024)
        .map(str::to_owned);

    if identity || player.pid.is_none() {
        player.pid = bus_call(connection, "GetConnectionUnixProcessID", &player.owner)
            .await
            .ok();
        if let Ok(root) = properties(connection, &player.owner, PREFIX).await {
            view.identity = clean(string(&root, "Identity").unwrap_or(&player.name), 80);
            view.can_raise = boolean(&root, "CanRaise");
            player.desktop_entry = string(&root, "DesktopEntry")
                .map(crate::window_rules::normalize_app_id)
                .filter(|value| !value.is_empty());
        }
    }

    Ok(())
}

async fn discover(connection: &Connection, name: String, owner: String) -> Player {
    let mut player = Player {
        view: ferese_ipc::media::Player {
            name: name.clone(),
            owner: owner.clone(),
            identity: clean(&name, 80),
            ..Default::default()
        },
        name,
        owner,
        ..Default::default()
    };
    let _ = refresh(connection, &mut player, true).await;
    player
}

async fn action(connection: &Connection, state: &mut PlayersState, args: &serde_json::Value) -> Result<(), String> {
    let kind = args["action"].as_str().ok_or("Missing media action")?;
    if kind == "auto" {
        state.pinned = None;
        return Ok(());
    }

    let player = match args["player"].as_str() {
        Some(name) => state.players.get(name),
        None => state.selected(),
    }
    .cloned()
    .ok_or("No active media player")?;
    if args["owner"].as_str().is_some_and(|owner| owner != player.owner) {
        return Err("Media player was replaced".into());
    }

    match kind {
        "pin" => {
            state.ignored.remove(&player.name);
            state.pinned = Some(player.name);
            return Ok(());
        }
        "ignore" => {
            let ignored = args["ignored"].as_bool().ok_or("Missing ignore state")?;
            if ignored {
                state.ignored.insert(player.name.clone());
            } else {
                state.ignored.remove(&player.name);
            }
            if ignored && state.pinned.as_deref() == Some(&player.name) {
                state.pinned = None;
            }
            return Ok(());
        }
        _ => (),
    }

    let view = &player.view;
    let supported = match kind {
        "play-pause" => view.can_toggle(),
        "next" => view.can_control && view.can_next,
        "previous" => view.can_control && view.can_previous,
        "raise" => view.can_raise,
        "seek" => view.can_control && view.can_seek && view.track_id.is_some(),
        "volume" => view.can_control && view.volume.is_some(),
        _ => false,
    };
    if !supported {
        return Err("Media action is unavailable for this player".into());
    }

    let method = match kind {
        "play-pause" => "PlayPause",
        "next" => "Next",
        "previous" => "Previous",
        "raise" => "Raise",
        _ => "",
    };
    let response = match kind {
        "seek" => {
            if args["track_id"].as_str() != view.track_id.as_deref() {
                return Err("Media track changed".into());
            }
            let position = args["position_us"].as_u64().ok_or("Invalid media position")?;
            let position = view
                .length_us
                .map_or(position, |length| position.min(length))
                .min(i64::MAX as u64) as i64;
            let track = OwnedObjectPath::try_from(view.track_id.clone().unwrap()).map_err(|error| error.to_string())?;
            connection
                .call_method(
                    Some(player.owner.as_str()),
                    PATH,
                    Some(PLAYER),
                    "SetPosition",
                    &(track, position),
                )
                .await
        }
        "volume" => {
            let delta = args["delta"]
                .as_f64()
                .filter(|delta| delta.is_finite() && delta.abs() <= 1.0)
                .ok_or("Invalid volume change")?;
            let volume = (view.volume.unwrap() + delta).clamp(0.0, 1.0);
            connection
                .call_method(
                    Some(player.owner.as_str()),
                    PATH,
                    Some(PROPERTIES),
                    "Set",
                    &(PLAYER, "Volume", Value::F64(volume)),
                )
                .await
        }
        _ => {
            connection
                .call_method(
                    Some(player.owner.as_str()),
                    PATH,
                    Some(if kind == "raise" { PREFIX } else { PLAYER }),
                    method,
                    &(),
                )
                .await
        }
    };
    response.map_err(|error| error.to_string())?;
    let mut player = player;
    if refresh(connection, &mut player, false).await.is_ok() {
        state.update(player);
    }
    Ok(())
}

fn publish(state: &PlayersState, previous: &mut Option<Update>, sender: &SyncSender<Update>) -> bool {
    let update = state.update_snapshot();
    if previous
        .as_ref()
        .is_some_and(|old| old.players == update.players && old.snapshot == update.snapshot)
    {
        return true;
    }
    *previous = Some(Update {
        players: update.players.clone(),
        snapshot: update.snapshot.clone(),
    });
    sender.send(update).is_ok()
}

async fn monitor(
    connection: Connection,
    commands: &async_channel::Receiver<Command>,
    sender: &SyncSender<Update>,
) -> zbus::Result<()> {
    let properties = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .path(PATH)?
        .interface(PROPERTIES)?
        .member("PropertiesChanged")?
        .build();
    let owners = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.freedesktop.DBus")?
        .interface("org.freedesktop.DBus")?
        .member("NameOwnerChanged")?
        .arg0ns(PREFIX)?
        .build();
    let seeks = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .path(PATH)?
        .interface(PLAYER)?
        .member("Seeked")?
        .build();
    let properties = MessageStream::for_match_rule(properties, &connection, Some(128)).await?;
    let owners = MessageStream::for_match_rule(owners, &connection, Some(128)).await?;
    let seeks = MessageStream::for_match_rule(seeks, &connection, Some(64)).await?;
    let mut signals = stream::or(stream::or(properties, owners), seeks);
    let mut state = PlayersState::default();
    let names: Vec<String> = connection
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "ListNames",
            &(),
        )
        .await?
        .body()
        .deserialize()?;
    let mut names: Vec<_> = names
        .into_iter()
        .filter(|name| name.strip_prefix(PREFIX).is_some_and(|suffix| suffix.starts_with('.')))
        .collect();
    names.sort_unstable();
    for name in names.into_iter().take(MAX_PLAYERS) {
        if let Ok(owner) = bus_call(&connection, "GetNameOwner", &name).await {
            let player = discover(&connection, name.clone(), owner).await;
            // Existing players have no known start order; bus names break ties.
            state.players.insert(name, player);
        }
    }

    let mut previous = None;
    if !publish(&state, &mut previous, sender) {
        return Ok(());
    }
    enum Input {
        Signal(Option<zbus::Result<zbus::Message>>),
        Action(Option<Command>),
    }
    loop {
        let input = match commands.try_recv() {
            Ok(command) => Input::Action(Some(command)),
            Err(_) => {
                future::or(async { Input::Signal(signals.next().await) }, async {
                    Input::Action(commands.recv().await.ok())
                })
                .await
            }
        };
        match input {
            Input::Action(None) => return Ok(()),
            Input::Signal(None) => return Err(zbus::Error::Failure("MPRIS signal stream ended".into())),
            Input::Action(Some(command)) => {
                let result = if command.sent.elapsed() > Duration::from_secs(2) {
                    Err("Media command expired".into())
                } else {
                    action(&connection, &mut state, &command.args).await
                };
                if let Some((id, response)) = command.reply {
                    let reply = match result {
                        Ok(()) => Response::success(id, serde_json::json!({})),
                        Err(error) => Response::error(id, "media_unavailable", error),
                    };
                    let _ = response.try_send(reply);
                }
            }
            Input::Signal(Some(message)) => {
                let message = message?;
                let header = message.header();
                if header
                    .member()
                    .is_some_and(|member| member.as_str() == "NameOwnerChanged")
                {
                    let (name, _, owner): (String, String, String) = message.body().deserialize()?;
                    if !name.strip_prefix(PREFIX).is_some_and(|suffix| suffix.starts_with('.')) {
                        continue;
                    }
                    state.remove(&name);
                    if !publish(&state, &mut previous, sender) {
                        return Ok(());
                    }
                    if !owner.is_empty() && state.players.len() < MAX_PLAYERS {
                        let player = discover(&connection, name, owner).await;
                        state.update(player);
                    }
                } else if let Some(owner) = header.sender() {
                    if header.member().is_some_and(|member| member.as_str() == "Seeked") {
                        let names: Vec<_> = state
                            .players
                            .values()
                            .filter(|player| player.owner == owner.as_str())
                            .map(|player| player.name.clone())
                            .collect();
                        for name in names {
                            let mut player = state.players[&name].clone();
                            if refresh(&connection, &mut player, false).await.is_ok() {
                                state.update(player);
                            }
                        }
                    } else {
                        let Ok((interface, changed, invalidated)) =
                            message.body().deserialize::<(String, Properties, Vec<String>)>()
                        else {
                            continue;
                        };
                        let identity = interface == PREFIX
                            && ["Identity", "DesktopEntry", "CanRaise"]
                                .iter()
                                .any(|key| changed.contains_key(*key) || invalidated.iter().any(|field| field == key));
                        let playback = interface == PLAYER
                            && [
                                "PlaybackStatus",
                                "Metadata",
                                "Rate",
                                "Volume",
                                "CanGoNext",
                                "CanGoPrevious",
                                "CanPlay",
                                "CanPause",
                                "CanSeek",
                                "CanControl",
                            ]
                            .iter()
                            .any(|key| changed.contains_key(*key) || invalidated.iter().any(|field| field == key));
                        if !identity && !playback {
                            continue;
                        }
                        let names: Vec<_> = state
                            .players
                            .values()
                            .filter(|player| player.owner == owner.as_str())
                            .map(|player| player.name.clone())
                            .collect();
                        for name in names {
                            let mut player = state.players[&name].clone();
                            // Re-read after a signal so queued startup messages cannot regress state.
                            if refresh(&connection, &mut player, identity).await.is_ok() {
                                state.update(player);
                            } else {
                                player.playing = false;
                                player.view.status = Playback::Stopped;
                                state.update(player);
                            }
                        }
                    }
                }
            }
        }

        if !publish(&state, &mut previous, sender) {
            return Ok(());
        }
    }
}

pub(super) fn run(sender: SyncSender<Update>, commands: async_channel::Receiver<Command>) {
    loop {
        let result = future::block_on(async {
            let connection = zbus::connection::Builder::session()?
                .method_timeout(Duration::from_millis(500))
                .build()
                .await?;
            monitor(connection, &commands, &sender).await
        });
        if matches!(result, Ok(())) || commands.is_closed() {
            break;
        }
        if sender.send(PlayersState::default().update_snapshot()).is_err() {
            break;
        }
        tracing::debug!(?result, "MPRIS disconnected; clearing playback and retrying");
        std::thread::sleep(Duration::from_secs(5));
    }
}

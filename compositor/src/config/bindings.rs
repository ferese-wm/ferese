use std::sync::Arc;

use super::*;

mod index;
pub(crate) use index::BindingSet;

impl Config {
    pub fn bindings(&self, input: &InputSettings) -> Result<Vec<Binding>, ConfigError> {
        let mut commands = HashMap::from([
            ("terminal".to_owned(), vec!["foot".to_owned()]),
            ("screenshot".to_owned(), vec!["ferese-screenshot".to_owned()]),
            (
                "screenshot-full".to_owned(),
                vec!["ferese-screenshot".to_owned(), "--full".to_owned()],
            ),
        ]);
        commands.extend(self.commands.clone());
        validate_commands(&commands)?;

        let keymap = physical_keymap(input)?;
        let mut bindings = default_bindings()
            .into_iter()
            .map(|binding| parse_binding(&binding, &commands, &keymap))
            .collect::<Result<Vec<_>, _>>()?;
        let mut supplied = HashSet::new();

        for configured in &self.bindings {
            let identity = binding_identity(configured, &keymap)?;
            if !supplied.insert(identity.clone()) {
                return Err(ConfigError::Binding(format!("duplicate binding {:?}", configured.keys)));
            }

            bindings.retain(|binding| binding_identity_for_runtime(binding) != identity);
            if configured.disabled {
                if configured.action.is_some() || configured.argument.is_some() {
                    return Err(ConfigError::Binding(format!(
                        "disabled binding {:?} cannot have an action or argument",
                        configured.keys
                    )));
                }
            } else {
                bindings.push(parse_binding(configured, &commands, &keymap)?);
            }
        }

        Ok(bindings)
    }
}

pub(super) fn validate_commands(commands: &HashMap<String, Vec<String>>) -> Result<(), ConfigError> {
    for (name, argv) in commands {
        if name.trim().is_empty() {
            return Err(ConfigError::Binding("command names cannot be empty".to_owned()));
        }
        if argv.is_empty() || argv[0].is_empty() {
            return Err(ConfigError::Binding(format!("command {name:?} must contain a program")));
        }
    }

    Ok(())
}

pub(crate) fn physical_keymap(input: &InputSettings) -> Result<xkb::Keymap, ConfigError> {
    let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    let options = (!input.xkb_options.is_empty()).then(|| input.xkb_options.join(","));

    xkb::Keymap::new_from_names(
        &context,
        "",
        "",
        &input.xkb_layout,
        &input.xkb_variant,
        options,
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
    .ok_or_else(|| ConfigError::Binding("failed to compile the XKB keymap".to_owned()))
}

pub(super) fn parse_binding(
    configured: &BindingConfig,
    commands: &HashMap<String, Vec<String>>,
    keymap: &xkb::Keymap,
) -> Result<Binding, ConfigError> {
    if configured.disabled {
        return Err(ConfigError::Binding(format!(
            "disabled binding {:?} cannot be executed",
            configured.keys
        )));
    }

    let identity = binding_identity(configured, keymap)?;
    let action = configured
        .action
        .as_deref()
        .ok_or_else(|| ConfigError::Binding(format!("binding {:?} has no action", configured.keys)))?;
    let action = parse_action(action, configured.argument.as_deref(), commands)?;
    let trigger = match identity.match_mode {
        BindingMatch::Keysym => BindingTrigger::Keysym(identity.key),
        BindingMatch::Physical => BindingTrigger::Physical(Keycode::new(identity.key)),
        BindingMatch::Swipe => BindingTrigger::Swipe {
            fingers: identity.key / 4,
            direction: [
                crate::gestures::SwipeDirection::Up,
                crate::gestures::SwipeDirection::Down,
                crate::gestures::SwipeDirection::Left,
                crate::gestures::SwipeDirection::Right,
            ][(identity.key % 4) as usize],
        },
    };

    Ok(Binding {
        modifiers: identity.modifiers,
        trigger,
        action,
    })
}

pub(super) fn binding_identity(
    configured: &BindingConfig,
    keymap: &xkb::Keymap,
) -> Result<BindingIdentity, ConfigError> {
    let (modifiers, key) = parse_chord(&configured.keys)?;
    if let Some((fingers, direction)) = parse_swipe(&key) {
        if configured.match_mode == BindingMatch::Physical || modifiers != BindingModifiers::default() {
            return Err(ConfigError::Binding(
                "swipes cannot use keyboard modifiers or physical key matching".into(),
            ));
        }
        return Ok(BindingIdentity {
            modifiers,
            match_mode: BindingMatch::Swipe,
            key: fingers * 4 + direction as u32,
        });
    }
    let key = match configured.match_mode {
        BindingMatch::Keysym => parse_keysym(&key)?,
        BindingMatch::Physical => keymap
            .key_by_name(&key.to_ascii_uppercase())
            .map(Keycode::raw)
            .ok_or_else(|| {
                ConfigError::Binding(format!(
                    "unknown XKB physical key name {key:?} in {:?}",
                    configured.keys
                ))
            })?,
        BindingMatch::Swipe => {
            return Err(ConfigError::Binding(
                "gesture keys must use Swipe3Left/Right/Up/Down (3–5 fingers)".into(),
            ));
        }
    };

    Ok(BindingIdentity {
        modifiers,
        match_mode: configured.match_mode,
        key,
    })
}

pub(super) fn binding_identity_for_runtime(binding: &Binding) -> BindingIdentity {
    let (match_mode, key) = match binding.trigger {
        BindingTrigger::Keysym(key) => (BindingMatch::Keysym, key),
        BindingTrigger::Physical(key) => (BindingMatch::Physical, key.raw()),
        BindingTrigger::Swipe { fingers, direction } => (BindingMatch::Swipe, fingers * 4 + direction as u32),
    };

    BindingIdentity {
        modifiers: binding.modifiers,
        match_mode,
        key,
    }
}

pub(super) fn parse_chord(chord: &str) -> Result<(BindingModifiers, String), ConfigError> {
    let mut modifiers = BindingModifiers::default();
    let mut key = None;

    for component in chord.split('+').map(str::trim) {
        if component.is_empty() {
            return Err(ConfigError::Binding(format!("invalid key chord {chord:?}")));
        }

        let slot = match component.to_ascii_lowercase().as_str() {
            "super" | "logo" | "mod4" => Some(&mut modifiers.logo),
            "ctrl" | "control" => Some(&mut modifiers.ctrl),
            "alt" => Some(&mut modifiers.alt),
            "shift" => Some(&mut modifiers.shift),
            _ => None,
        };
        if let Some(slot) = slot {
            if *slot {
                return Err(ConfigError::Binding(format!("duplicate modifier in {chord:?}")));
            }
            *slot = true;
        } else if key.replace(component.to_owned()).is_some() {
            return Err(ConfigError::Binding(format!(
                "key chord {chord:?} contains more than one key"
            )));
        }
    }

    let key = key.ok_or_else(|| ConfigError::Binding(format!("key chord {chord:?} does not contain a key")))?;
    Ok((modifiers, key))
}

pub(super) fn parse_swipe(key: &str) -> Option<(u32, crate::gestures::SwipeDirection)> {
    use crate::gestures::SwipeDirection;
    let lower = key.to_ascii_lowercase();
    let rest = lower.strip_prefix("swipe")?;
    let fingers = rest.get(..1)?.parse::<u32>().ok()?;
    if !(3..=5).contains(&fingers) {
        return None;
    }
    let direction = match rest.get(1..)? {
        "up" => SwipeDirection::Up,
        "down" => SwipeDirection::Down,
        "left" => SwipeDirection::Left,
        "right" => SwipeDirection::Right,
        _ => return None,
    };
    Some((fingers, direction))
}

pub(super) fn parse_keysym(name: &str) -> Result<u32, ConfigError> {
    let normalized = match name {
        "Enter" | "enter" => "Return".to_owned(),
        "Space" | "space" => "space".to_owned(),
        "[" => "bracketleft".to_owned(),
        "]" => "bracketright".to_owned(),
        name if name.len() == 1 => name.to_ascii_lowercase(),
        name => name.to_owned(),
    };
    let mut symbol = xkb::keysym_from_name(&normalized, xkb::KEYSYM_NO_FLAGS);
    if symbol.raw() == keysyms::KEY_NoSymbol {
        symbol = xkb::keysym_from_name(&normalized, xkb::KEYSYM_CASE_INSENSITIVE);
    }
    if symbol.raw() == keysyms::KEY_NoSymbol {
        Err(ConfigError::Binding(format!("unknown keysym {name:?}")))
    } else {
        Ok(symbol.raw())
    }
}

pub(super) fn parse_action(
    action: &str,
    argument: Option<&str>,
    commands: &HashMap<String, Vec<String>>,
) -> Result<BindingAction, ConfigError> {
    let no_argument = || {
        if argument.is_some() {
            Err(ConfigError::Binding(format!(
                "action {action:?} does not accept an argument"
            )))
        } else {
            Ok(())
        }
    };
    let required_argument =
        || argument.ok_or_else(|| ConfigError::Binding(format!("action {action:?} requires an argument")));

    match action {
        "none" => {
            no_argument()?;
            Ok(BindingAction::None)
        }
        "workspace-next" | "workspace-previous" => {
            no_argument()?;
            Ok(BindingAction::SwitchRelativeWorkspace(action == "workspace-next"))
        }
        "workspace-back-and-forth" => {
            no_argument()?;
            Ok(BindingAction::WorkspaceBackAndForth)
        }
        "focus-last-window" => {
            no_argument()?;
            Ok(BindingAction::FocusLastWindow)
        }
        "focus-mru-next" | "focus-mru-previous" => {
            no_argument()?;
            Ok(BindingAction::FocusMru(action == "focus-mru-previous"))
        }
        "spawn" => {
            let command = required_argument()?;
            let argv = commands
                .get(command)
                .ok_or_else(|| ConfigError::Binding(format!("unknown command {command:?}")))?;
            Ok(BindingAction::Spawn(argv.clone().into()))
        }
        "close" => {
            no_argument()?;
            Ok(BindingAction::Close)
        }
        "exit" => {
            no_argument()?;
            Ok(BindingAction::Exit)
        }
        "focus" => Ok(BindingAction::Focus(parse_direction(required_argument()?)?)),
        "move" => Ok(BindingAction::Move(parse_direction(required_argument()?)?)),
        "resize" => Ok(BindingAction::Resize(parse_direction(required_argument()?)?)),
        "workspace" => Ok(BindingAction::SwitchWorkspace(parse_workspace(required_argument()?)?)),
        "move-to-workspace" => Ok(BindingAction::MoveToWorkspace(parse_workspace(required_argument()?)?)),
        "toggle-maximized" => {
            no_argument()?;
            Ok(BindingAction::ToggleMaximized)
        }
        "toggle-fullscreen" => {
            no_argument()?;
            Ok(BindingAction::ToggleFullscreen)
        }
        "toggle-layout" => {
            no_argument()?;
            Ok(BindingAction::ToggleLayout)
        }
        "cycle-column-width" => {
            no_argument()?;
            Ok(BindingAction::CycleColumnWidth)
        }
        "center-column" => {
            no_argument()?;
            Ok(BindingAction::CenterColumn)
        }
        "consume" => {
            no_argument()?;
            Ok(BindingAction::Consume)
        }
        "expel" => {
            no_argument()?;
            Ok(BindingAction::Expel)
        }
        "toggle-floating" => {
            no_argument()?;
            Ok(BindingAction::ToggleFloating)
        }
        "toggle-overview" => {
            no_argument()?;
            Ok(BindingAction::ToggleOverview)
        }
        "media-play-pause" | "media-next" | "media-previous" => {
            no_argument()?;
            Ok(BindingAction::Media(match action {
                "media-next" => "next",
                "media-previous" => "previous",
                _ => "play-pause",
            }))
        }
        "toggle-display-mode" => {
            no_argument()?;
            Ok(BindingAction::ToggleDisplayMode)
        }
        "toggle-keybinding-guide" => {
            no_argument()?;
            Ok(BindingAction::ToggleKeybindingGuide)
        }
        _ => Err(ConfigError::Binding(format!("unknown action {action:?}"))),
    }
}

pub(super) fn parse_direction(argument: &str) -> Result<Direction, ConfigError> {
    match argument {
        "left" => Ok(Direction::Left),
        "right" => Ok(Direction::Right),
        "up" => Ok(Direction::Up),
        "down" => Ok(Direction::Down),
        _ => Err(ConfigError::Binding(format!("invalid direction {argument:?}"))),
    }
}

pub(super) fn parse_workspace(argument: &str) -> Result<u8, ConfigError> {
    match argument.parse() {
        Ok(workspace) if workspace > 0 => Ok(workspace),
        _ => Err(ConfigError::Binding(format!("invalid workspace {argument:?}"))),
    }
}

pub(super) fn default_bindings() -> Vec<BindingConfig> {
    let mut bindings = vec![
        binding("Super+Enter", "spawn", Some("terminal")),
        binding("Super+Shift+S", "spawn", Some("screenshot")),
        binding("Print", "spawn", Some("screenshot-full")),
        binding("Swipe3Up", "workspace-next", None),
        binding("Swipe3Down", "workspace-previous", None),
        binding("Swipe3Left", "focus", Some("right")),
        binding("Swipe3Right", "focus", Some("left")),
        binding("Super+Q", "close", None),
        binding("Super+F", "toggle-maximized", None),
        binding("Super+Shift+F", "toggle-fullscreen", None),
        binding("Super+M", "toggle-layout", None),
        binding("Super+R", "cycle-column-width", None),
        binding("Super+C", "center-column", None),
        binding("Super+[", "consume", None),
        binding("Super+]", "expel", None),
        binding("Super+Shift+Space", "toggle-floating", None),
        binding("Super+Tab", "toggle-overview", None),
        binding("XF86Display", "toggle-display-mode", None),
        binding("XF86AudioPlay", "media-play-pause", None),
        binding("XF86AudioPause", "media-play-pause", None),
        binding("XF86AudioNext", "media-next", None),
        binding("XF86AudioPrev", "media-previous", None),
        binding("Super+F1", "toggle-keybinding-guide", None),
        binding("Super+Escape", "workspace-back-and-forth", None),
        binding("Super+BackSpace", "focus-last-window", None),
        binding("Alt+Tab", "focus-mru-next", None),
        binding("Alt+Shift+Tab", "focus-mru-previous", None),
        binding("Super+Shift+E", "exit", None),
    ];

    for (key, direction) in [("H", "left"), ("J", "down"), ("K", "up"), ("L", "right")] {
        bindings.push(binding("Super+".to_owned() + key, "focus", Some(direction)));
        bindings.push(binding("Super+Shift+".to_owned() + key, "move", Some(direction)));
        bindings.push(binding("Super+Ctrl+".to_owned() + key, "resize", Some(direction)));
    }

    for workspace in 1..=9 {
        let workspace = workspace.to_string();
        bindings.push(binding(format!("Super+{workspace}"), "workspace", Some(&workspace)));
        bindings.push(binding(
            format!("Super+Shift+{workspace}"),
            "move-to-workspace",
            Some(&workspace),
        ));
    }

    bindings
}

pub(super) fn binding(keys: impl Into<String>, action: impl Into<String>, argument: Option<&str>) -> BindingConfig {
    BindingConfig {
        keys: keys.into(),
        match_mode: BindingMatch::Keysym,
        action: Some(action.into()),
        argument: argument.map(str::to_owned),
        disabled: false,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    pub(super) modifiers: BindingModifiers,
    pub(super) trigger: BindingTrigger,
    pub action: BindingAction,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum BindingTrigger {
    Keysym(u32),
    Physical(Keycode),
    Swipe {
        fingers: u32,
        direction: crate::gestures::SwipeDirection,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub(super) struct BindingModifiers {
    pub(super) logo: bool,
    pub(super) ctrl: bool,
    pub(super) alt: bool,
    pub(super) shift: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum BindingAction {
    None,
    Spawn(Arc<[String]>),
    Close,
    Exit,
    Focus(ferese_layout::Direction),
    FocusLastWindow,
    FocusMru(bool),
    Move(ferese_layout::Direction),
    Resize(ferese_layout::Direction),
    SwitchWorkspace(u8),
    SwitchRelativeWorkspace(bool),
    WorkspaceBackAndForth,
    MoveToWorkspace(u8),
    ToggleFullscreen,
    ToggleMaximized,
    ToggleLayout,
    CycleColumnWidth,
    CenterColumn,
    Consume,
    Expel,
    ToggleFloating,
    ToggleOverview,
    ToggleKeybindingGuide,
    ToggleDisplayMode,
    Media(&'static str),
}

impl Binding {
    pub(crate) fn portal_trigger(chord: &str) -> Result<Self, ConfigError> {
        if chord.len() > 256 {
            return Err(ConfigError::Binding("Shortcut is too long".into()));
        }
        let parts = chord.split('+').collect::<Vec<_>>();
        if parts
            .iter()
            .filter(|part| part.trim().eq_ignore_ascii_case("num"))
            .count()
            > 1
        {
            return Err(ConfigError::Binding("Duplicate NUM modifier".into()));
        }
        let chord = parts
            .into_iter()
            .filter(|part| !part.trim().eq_ignore_ascii_case("num"))
            .collect::<Vec<_>>()
            .join("+");
        let (modifiers, key) = parse_chord(&chord)?;
        let symbol = parse_keysym(&key)?;
        if (modifiers.ctrl
            && modifiers.alt
            && matches!(symbol, keysyms::KEY_Escape | keysyms::KEY_F1..=keysyms::KEY_F12))
            || (keysyms::KEY_XF86Switch_VT_1..=keysyms::KEY_XF86Switch_VT_12).contains(&symbol)
        {
            return Err(ConfigError::Binding("Reserved system shortcut".into()));
        }
        Ok(Self {
            modifiers,
            trigger: BindingTrigger::Keysym(symbol),
            action: BindingAction::None,
        })
    }

    pub(crate) fn conflicts(&self, other: &Self, map: &xkb::Keymap) -> bool {
        if self.modifiers != other.modifiers {
            return false;
        }
        match (&self.trigger, &other.trigger) {
            (BindingTrigger::Keysym(left), BindingTrigger::Keysym(right)) => left == right,
            (BindingTrigger::Keysym(symbol), BindingTrigger::Physical(code))
            | (BindingTrigger::Physical(code), BindingTrigger::Keysym(symbol)) => (0..map.num_layouts_for_key(*code))
                .any(|layout| {
                    map.key_get_syms_by_level(*code, layout, 0)
                        .iter()
                        .any(|key| key.raw() == *symbol)
                }),
            (BindingTrigger::Physical(left), BindingTrigger::Physical(right)) => left == right,
            _ => false,
        }
    }

    pub fn matches(&self, keycode: Keycode, keysyms: &[u32], logo: bool, ctrl: bool, alt: bool, shift: bool) -> bool {
        self.modifiers == BindingModifiers { logo, ctrl, alt, shift }
            && match self.trigger {
                BindingTrigger::Keysym(expected) => keysyms.contains(&expected),
                BindingTrigger::Physical(expected) => expected == keycode,
                BindingTrigger::Swipe { .. } => false,
            }
    }

    pub(crate) fn guide_entry(&self, keymap: Option<&xkb::Keymap>) -> Option<serde_json::Value> {
        let description = match &self.action {
            BindingAction::None => return None,
            BindingAction::Spawn(argv) if argv.first().is_some_and(|program| program == "ferese-screenshot") => {
                if argv.iter().any(|arg| arg == "--full") {
                    "Capture whole screen"
                } else {
                    "Capture selected area"
                }
                .into()
            }
            BindingAction::Spawn(argv) => format!(
                "Launch {}",
                argv.first()
                    .and_then(|program| std::path::Path::new(program).file_name())
                    .and_then(|name| name.to_str())
                    .unwrap_or("application")
            ),
            BindingAction::Close => "Close window".into(),
            BindingAction::Exit => "Log out (with confirmation)".into(),
            BindingAction::Focus(direction) => format!("Focus window {direction:?}"),
            BindingAction::FocusLastWindow => "Return to last focused window".into(),
            BindingAction::FocusMru(reverse) => if *reverse {
                "Previous window in focus history"
            } else {
                "Next window in focus history"
            }
            .into(),
            BindingAction::Move(direction) => format!("Move window {direction:?}"),
            BindingAction::Resize(direction) => format!("Resize window {direction:?}"),
            BindingAction::SwitchWorkspace(index) => format!("Go to workspace {index}"),
            BindingAction::WorkspaceBackAndForth => "Return to last workspace on this monitor".into(),
            BindingAction::MoveToWorkspace(index) => format!("Move window to workspace {index}"),
            BindingAction::SwitchRelativeWorkspace(next) => {
                if *next { "Next workspace" } else { "Previous workspace" }.into()
            }
            BindingAction::ToggleFullscreen => "Toggle fullscreen".into(),
            BindingAction::ToggleMaximized => "Toggle full-width window".into(),
            BindingAction::ToggleLayout => "Switch workspace layout".into(),
            BindingAction::CycleColumnWidth => "Cycle column width".into(),
            BindingAction::CenterColumn => "Center focused column".into(),
            BindingAction::Consume => "Join window into column".into(),
            BindingAction::Expel => "Move window out of column".into(),
            BindingAction::ToggleFloating => "Toggle floating window".into(),
            BindingAction::Media(action) => match *action {
                "next" => "Next track",
                "previous" => "Previous track",
                _ => "Play or pause media",
            }
            .into(),
            BindingAction::ToggleOverview => "Open or close overview".into(),
            BindingAction::ToggleDisplayMode => "Open display mode chooser".into(),
            BindingAction::ToggleKeybindingGuide => "Open or close shortcut hint".into(),
        };
        let mut parts = Vec::new();
        for (enabled, label) in [
            (self.modifiers.logo, "Super"),
            (self.modifiers.ctrl, "Ctrl"),
            (self.modifiers.alt, "Alt"),
            (self.modifiers.shift, "Shift"),
        ] {
            if enabled {
                parts.push(label.to_owned());
            }
        }
        parts.push(match self.trigger {
            BindingTrigger::Keysym(symbol) => {
                let name = xkb::keysym_get_name(xkb::Keysym::new(symbol));
                match name.as_str() {
                    "Return" => "Enter".into(),
                    "Print" => "Print Screen".into(),
                    "space" => "Space".into(),
                    _ if name.len() == 1 => name.to_uppercase(),
                    _ => name,
                }
            }
            BindingTrigger::Physical(code) => keymap
                .and_then(|map| map.key_get_name(code))
                .map(|name| format!("Physical {name}"))
                .unwrap_or_else(|| format!("Keycode {}", u32::from(code))),
            BindingTrigger::Swipe { fingers, direction } => {
                format!("{fingers}-finger swipe {direction:?}")
            }
        });
        Some(serde_json::json!({"keys": parts.join(" + "), "description": description}))
    }

    pub(crate) fn swipe_fingers(&self, count: u32) -> bool {
        matches!(self.trigger, BindingTrigger::Swipe { fingers, .. } if fingers == count)
    }

    pub(crate) fn matches_swipe(&self, count: u32, target: crate::gestures::SwipeDirection) -> bool {
        matches!(self.trigger, BindingTrigger::Swipe { fingers, direction } if fingers == count && direction == target)
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq)]
#[serde(rename_all = "lowercase")]
pub(super) enum BindingMatch {
    #[default]
    Keysym,
    Physical,
    Swipe,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct BindingConfig {
    pub(super) keys: String,
    #[serde(default, rename = "match")]
    pub(super) match_mode: BindingMatch,
    pub(super) action: Option<String>,
    pub(super) argument: Option<String>,
    #[serde(default)]
    pub(super) disabled: bool,
}

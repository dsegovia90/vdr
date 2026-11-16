//! Keyboard shortcuts: what each action is, which key does it, and the user's
//! changes to that.
//!
//! **One table, no UI in it.** The page's key handler turns a keyboard event
//! into a [`Chord`] and asks the [`Keymap`] what it means; the settings tab
//! reads and edits the same map. Kept free of Dioxus so a system-wide key
//! source could feed the same table later, and so the rules here are tested
//! without a window.
//!
//! **Chords are written as text people can edit**: lowercase, modifiers then
//! the key, joined by `+` -- `m`, `cmd+shift+l`, `ctrl+,`, `f5`, `space`. That
//! is the form `settings.json` stores and the log prints.
//!
//! Esc is not here: it closes things, everywhere, and cannot be rebound.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

/// Where an action is listed in the settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Section {
    InACall,
    App,
}

impl Section {
    /// Every section, in the order the settings list them.
    pub const ALL: [Section; 2] = [Section::InACall, Section::App];

    /// The heading, as written; the settings show it in capitals.
    pub fn title(self) -> &'static str {
        match self {
            Section::InACall => "In a call",
            Section::App => "App",
        }
    }
}

/// Something a key can do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Action {
    ToggleMic,
    ToggleCamera,
    ShareScreen,
    Leave,
    OpenSettings,
}

impl Action {
    /// Every action, in the order the settings list them. A lookup that finds
    /// two actions on one chord (only possible in a hand-edited file) takes
    /// the first.
    pub const ALL: [Action; 5] = [
        Action::ToggleMic,
        Action::ToggleCamera,
        Action::ShareScreen,
        Action::Leave,
        Action::OpenSettings,
    ];

    /// The name `settings.json` stores it under. Never change one: a renamed
    /// id silently drops everyone's binding for it.
    pub fn id(self) -> &'static str {
        match self {
            Action::ToggleMic => "toggle_mic",
            Action::ToggleCamera => "toggle_camera",
            Action::ShareScreen => "share_screen",
            Action::Leave => "leave",
            Action::OpenSettings => "open_settings",
        }
    }

    pub fn from_id(id: &str) -> Option<Action> {
        Action::ALL.into_iter().find(|action| action.id() == id)
    }

    pub fn title(self) -> &'static str {
        match self {
            Action::ToggleMic => "Toggle microphone",
            Action::ToggleCamera => "Toggle camera",
            Action::ShareScreen => "Share screen",
            Action::Leave => "Leave call",
            Action::OpenSettings => "Open settings",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Action::ToggleMic => "Mute or unmute, as the microphone button does",
            Action::ToggleCamera => "Turn the camera off or on",
            Action::ShareScreen => "Stop sharing, or open the list of screens to share",
            Action::Leave => "Leave the call, or cancel one still connecting",
            Action::OpenSettings => "Show this window",
        }
    }

    pub fn section(self) -> Section {
        match self {
            Action::OpenSettings => Section::App,
            _ => Section::InACall,
        }
    }

    /// The key it has until someone changes it. `cmd` on macOS, `ctrl`
    /// elsewhere, as each platform's other apps do.
    pub fn default_chord(self) -> Chord {
        let action = if cfg!(target_os = "macos") {
            Mods::CMD
        } else {
            Mods::CTRL
        };
        match self {
            Action::ToggleMic => Chord::plain(Key::Char('m')),
            Action::ToggleCamera => Chord::plain(Key::Char('v')),
            Action::ShareScreen => Chord::plain(Key::Char('s')),
            Action::Leave => Chord::new(action.with(Mods::SHIFT), Key::Char('l')),
            Action::OpenSettings => Chord::new(action, Key::Char(',')),
        }
    }
}

/// Which modifiers are held. `cmd` is the Command key on macOS and the
/// Super/Windows key elsewhere.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Mods {
    pub cmd: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Mods {
    pub const NONE: Mods = Mods { cmd: false, ctrl: false, alt: false, shift: false };
    pub const CMD: Mods = Mods { cmd: true, ..Mods::NONE };
    pub const CTRL: Mods = Mods { ctrl: true, ..Mods::NONE };
    pub const SHIFT: Mods = Mods { shift: true, ..Mods::NONE };

    pub const fn with(self, other: Mods) -> Mods {
        Mods {
            cmd: self.cmd || other.cmd,
            ctrl: self.ctrl || other.ctrl,
            alt: self.alt || other.alt,
            shift: self.shift || other.shift,
        }
    }
}

/// A key that is not a modifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Key {
    /// A printable character, lowercase where it has a case.
    Char(char),
    Named(Named),
    /// A function key, `f1` to `f24`.
    F(u8),
}

/// The named keys a chord can use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Named {
    Space,
    Enter,
    Tab,
    Backspace,
    Delete,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
}

impl Named {
    const ALL: [Named; 15] = [
        Named::Space,
        Named::Enter,
        Named::Tab,
        Named::Backspace,
        Named::Delete,
        Named::Escape,
        Named::Up,
        Named::Down,
        Named::Left,
        Named::Right,
        Named::Home,
        Named::End,
        Named::PageUp,
        Named::PageDown,
        Named::Insert,
    ];

    /// How it is written in a chord.
    pub fn name(self) -> &'static str {
        match self {
            Named::Space => "space",
            Named::Enter => "enter",
            Named::Tab => "tab",
            Named::Backspace => "backspace",
            Named::Delete => "delete",
            Named::Escape => "esc",
            Named::Up => "up",
            Named::Down => "down",
            Named::Left => "left",
            Named::Right => "right",
            Named::Home => "home",
            Named::End => "end",
            Named::PageUp => "pageup",
            Named::PageDown => "pagedown",
            Named::Insert => "insert",
        }
    }

    /// How a key cap shows it.
    fn label(self) -> &'static str {
        match self {
            Named::Space => "Space",
            Named::Enter => "Enter",
            Named::Tab => "Tab",
            Named::Backspace => "Backspace",
            Named::Delete => "Delete",
            Named::Escape => "Esc",
            Named::Up => "\u{2191}",
            Named::Down => "\u{2193}",
            Named::Left => "\u{2190}",
            Named::Right => "\u{2192}",
            Named::Home => "Home",
            Named::End => "End",
            Named::PageUp => "PgUp",
            Named::PageDown => "PgDn",
            Named::Insert => "Insert",
        }
    }

    fn parse(text: &str) -> Option<Named> {
        let alias = match text {
            "escape" => Some(Named::Escape),
            "return" => Some(Named::Enter),
            "del" => Some(Named::Delete),
            _ => None,
        };
        alias.or_else(|| Named::ALL.into_iter().find(|named| named.name() == text))
    }
}

impl Key {
    /// A typed character as a key: lowercased, so `L` and `l` are one key and
    /// Shift is the modifier's business. Whitespace has its own names.
    pub fn from_char(c: char) -> Option<Key> {
        match c {
            ' ' => Some(Key::Named(Named::Space)),
            '\n' | '\r' => Some(Key::Named(Named::Enter)),
            '\t' => Some(Key::Named(Named::Tab)),
            c if c.is_control() || c.is_whitespace() => None,
            c => Some(Key::Char(c.to_lowercase().next().unwrap_or(c))),
        }
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // `+` is the separator, so it goes by a name.
            Key::Char('+') => f.write_str("plus"),
            Key::Char(c) => write!(f, "{c}"),
            Key::Named(named) => f.write_str(named.name()),
            Key::F(n) => write!(f, "f{n}"),
        }
    }
}

impl FromStr for Key {
    type Err = ChordError;

    fn from_str(text: &str) -> Result<Key, ChordError> {
        if text == "plus" {
            return Ok(Key::Char('+'));
        }
        if let Some(named) = Named::parse(text) {
            return Ok(Key::Named(named));
        }
        if let Some(n) = text.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
            if (1..=24).contains(&n) {
                return Ok(Key::F(n));
            }
        }
        let mut chars = text.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => Key::from_char(c).ok_or_else(|| ChordError::UnknownKey(text.to_string())),
            _ => Err(ChordError::UnknownKey(text.to_string())),
        }
    }
}

/// A key with the modifiers held for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Chord {
    pub mods: Mods,
    pub key: Key,
}

/// Why a chord's text did not parse.
#[derive(Clone, Debug, PartialEq)]
pub enum ChordError {
    Empty,
    UnknownKey(String),
    UnknownModifier(String),
}

impl fmt::Display for ChordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChordError::Empty => f.write_str("no key"),
            ChordError::UnknownKey(key) => write!(f, "unknown key {key:?}"),
            ChordError::UnknownModifier(name) => write!(f, "unknown modifier {name:?}"),
        }
    }
}

impl Chord {
    pub const fn new(mods: Mods, key: Key) -> Chord {
        Chord { mods, key }
    }

    pub const fn plain(key: Key) -> Chord {
        Chord::new(Mods::NONE, key)
    }

    /// A chord as a keyboard reports it: the key it produced, and what was
    /// held. Shift is dropped for a character that has no case, because the
    /// character already carries it -- Shift+/ arrives as `?`, and is stored
    /// as `?`, not `shift+?`, so that the binding and the press agree.
    pub fn pressed(mods: Mods, key: Key) -> Chord {
        let mods = match key {
            Key::Char(c) if !c.is_alphabetic() => Mods { shift: false, ..mods },
            _ => mods,
        };
        Chord::new(mods, key)
    }

    /// Whether this is Esc with nothing held: it closes things and is never
    /// a binding.
    pub fn is_escape(&self) -> bool {
        self.key == Key::Named(Named::Escape) && self.mods == Mods::NONE
    }

    /// What a key cap shows: `⌃⌥⇧⌘L` on macOS, in Apple's order, and
    /// `Ctrl+Alt+Shift+Super+L` elsewhere.
    pub fn label(&self, mac: bool) -> String {
        let key = match self.key {
            Key::Char(c) => c.to_uppercase().collect(),
            Key::Named(named) => named.label().to_string(),
            Key::F(n) => format!("F{n}"),
        };
        let Mods { cmd, ctrl, alt, shift } = self.mods;
        if mac {
            let mut out = String::new();
            for (held, symbol) in [(ctrl, "\u{2303}"), (alt, "\u{2325}"), (shift, "\u{21E7}"), (cmd, "\u{2318}")] {
                if held {
                    out.push_str(symbol);
                }
            }
            out + &key
        } else {
            let mut parts: Vec<&str> = Vec::new();
            for (held, name) in [(ctrl, "Ctrl"), (alt, "Alt"), (shift, "Shift"), (cmd, "Super")] {
                if held {
                    parts.push(name);
                }
            }
            parts.push(&key);
            parts.join("+")
        }
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Mods { cmd, ctrl, alt, shift } = self.mods;
        for (held, name) in [(cmd, "cmd"), (ctrl, "ctrl"), (alt, "alt"), (shift, "shift")] {
            if held {
                write!(f, "{name}+")?;
            }
        }
        write!(f, "{}", self.key)
    }
}

impl FromStr for Chord {
    type Err = ChordError;

    /// Lenient about case, spaces and modifier spellings (`command`, `super`,
    /// `meta`, `control`, `option`), strict about everything else.
    fn from_str(text: &str) -> Result<Chord, ChordError> {
        let text: String = text.trim().to_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
        if text.is_empty() {
            return Err(ChordError::Empty);
        }
        let mut parts: Vec<&str> = text.split('+').collect();
        // `ctrl++`: the key was a plus, split into two empty pieces.
        if text.ends_with("++") {
            parts.truncate(parts.len() - 2);
            parts.push("plus");
        }
        let Some(key) = parts.pop() else { return Err(ChordError::Empty) };
        if key.is_empty() {
            return Err(ChordError::Empty);
        }
        let mut mods = Mods::NONE;
        for part in parts {
            match part {
                "cmd" | "command" | "super" | "meta" | "win" => mods.cmd = true,
                "ctrl" | "control" => mods.ctrl = true,
                "alt" | "option" | "opt" => mods.alt = true,
                "shift" => mods.shift = true,
                other => return Err(ChordError::UnknownModifier(other.to_string())),
            }
        }
        Ok(Chord::new(mods, key.parse()?))
    }
}

/// Why a binding was refused.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Refusal {
    /// Another action has it.
    Taken(Action),
    /// Esc, which closes things everywhere and cannot be bound.
    Reserved,
}

/// Every action's key: the defaults, with the user's changes over them.
///
/// Only changes are kept, so a default that changes in a later version
/// reaches everyone who never touched it. `None` in the overrides is a key
/// the user removed on purpose.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Keymap {
    overrides: BTreeMap<Action, Option<Chord>>,
}

impl Keymap {
    /// The map from `settings.json`'s `hotkeys`: action id to chord text, an
    /// empty string meaning unbound.
    ///
    /// **Nothing in the file can break the app.** An unknown id (a newer
    /// build's action, a typo) and a chord that does not parse are skipped
    /// with a log line, and the action keeps its default. A file that gives
    /// one key to two actions loads, and the first in [`Action::ALL`] wins.
    pub fn from_overrides(saved: &BTreeMap<String, String>) -> Keymap {
        let mut keymap = Keymap::default();
        for (id, text) in saved {
            let Some(action) = Action::from_id(id) else {
                tracing::warn!("settings: ignoring hotkey for unknown action {id:?}");
                continue;
            };
            if text.trim().is_empty() {
                keymap.overrides.insert(action, None);
                continue;
            }
            match text.parse::<Chord>() {
                Ok(chord) if chord.is_escape() => {
                    tracing::warn!("settings: esc cannot be bound (for {id}); using the default");
                }
                Ok(chord) => {
                    keymap.overrides.insert(action, Some(chord));
                }
                Err(error) => {
                    tracing::warn!("settings: ignoring hotkey {text:?} for {id}: {error}");
                }
            }
        }
        for action in Action::ALL {
            if let Some(chord) = keymap.chord(action) {
                if let Some(first) = keymap.lookup(chord).filter(|first| *first != action) {
                    tracing::warn!(
                        "settings: {chord} is bound to both {} and {}; {} wins",
                        first.id(),
                        action.id(),
                        first.id()
                    );
                }
            }
        }
        keymap
    }

    /// What `settings.json` stores: the changes only.
    pub fn overrides(&self) -> BTreeMap<String, String> {
        self.overrides
            .iter()
            .map(|(action, chord)| (action.id().to_string(), chord.map(|chord| chord.to_string()).unwrap_or_default()))
            .collect()
    }

    /// The action's key, if it has one.
    pub fn chord(&self, action: Action) -> Option<Chord> {
        match self.overrides.get(&action) {
            Some(chord) => *chord,
            None => Some(action.default_chord()),
        }
    }

    pub fn is_default(&self, action: Action) -> bool {
        !self.overrides.contains_key(&action)
    }

    /// Which action a chord means.
    pub fn lookup(&self, chord: Chord) -> Option<Action> {
        Action::ALL.into_iter().find(|action| self.chord(*action) == Some(chord))
    }

    /// The action other than `action` that already has `chord`.
    pub fn conflict(&self, action: Action, chord: Chord) -> Option<Action> {
        Action::ALL
            .into_iter()
            .find(|other| *other != action && self.chord(*other) == Some(chord))
    }

    /// Gives `action` the key `chord`, unless something else has it.
    pub fn set(&mut self, action: Action, chord: Chord) -> Result<(), Refusal> {
        if chord.is_escape() {
            return Err(Refusal::Reserved);
        }
        if let Some(other) = self.conflict(action, chord) {
            return Err(Refusal::Taken(other));
        }
        if chord == action.default_chord() {
            self.overrides.remove(&action);
        } else {
            self.overrides.insert(action, Some(chord));
        }
        Ok(())
    }

    /// Leaves `action` with no key.
    pub fn unbind(&mut self, action: Action) {
        self.overrides.insert(action, None);
    }

    /// Back to the default key -- refused if another action has taken it
    /// meanwhile, rather than leaving one key on two actions.
    pub fn reset(&mut self, action: Action) -> Result<(), Refusal> {
        self.set(action, action.default_chord())
    }

    pub fn reset_all(&mut self) {
        self.overrides.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chord(text: &str) -> Chord {
        text.parse().unwrap()
    }

    #[test]
    fn chords_parse_and_print_the_same() {
        for text in ["m", "cmd+shift+l", "ctrl+,", "f5", "space", "enter", "left", "alt+pageup", "ctrl+plus", "cmd+ctrl+alt+shift+k"] {
            assert_eq!(chord(text).to_string(), text, "{text}");
        }
    }

    #[test]
    fn parsing_is_lenient_about_spelling_and_order() {
        assert_eq!(chord("Shift + Command + L"), chord("cmd+shift+l"));
        assert_eq!(chord("control+option+x"), chord("ctrl+alt+x"));
        assert_eq!(chord("super+m"), chord("cmd+m"));
        assert_eq!(chord("escape"), chord("esc"));
        assert_eq!(chord("ctrl++"), chord("ctrl+plus"));
        assert_eq!(chord("M"), chord("m"));
    }

    #[test]
    fn nonsense_does_not_parse() {
        assert_eq!("".parse::<Chord>(), Err(ChordError::Empty));
        assert_eq!("cmd+".parse::<Chord>(), Err(ChordError::Empty));
        assert!(matches!("hyper+m".parse::<Chord>(), Err(ChordError::UnknownModifier(_))));
        assert!(matches!("cmd+banana".parse::<Chord>(), Err(ChordError::UnknownKey(_))));
        assert!(matches!("f25".parse::<Chord>(), Err(ChordError::UnknownKey(_))));
    }

    #[test]
    fn a_shifted_symbol_is_its_own_key() {
        let pressed = Chord::pressed(Mods::SHIFT, Key::Char('?'));
        assert_eq!(pressed.to_string(), "?");
        let letter = Chord::pressed(Mods::SHIFT.with(Mods::CMD), Key::from_char('L').unwrap());
        assert_eq!(letter.to_string(), "cmd+shift+l");
    }

    #[test]
    fn caps_read_as_each_platform_writes_them() {
        assert_eq!(chord("cmd+shift+l").label(true), "\u{21E7}\u{2318}L");
        assert_eq!(chord("ctrl+shift+l").label(false), "Ctrl+Shift+L");
        assert_eq!(chord("ctrl+,").label(false), "Ctrl+,");
        assert_eq!(chord("space").label(true), "Space");
    }

    #[test]
    fn the_defaults_are_all_different() {
        let keymap = Keymap::default();
        for action in Action::ALL {
            assert_eq!(keymap.lookup(action.default_chord()), Some(action));
        }
        let leave = if cfg!(target_os = "macos") { "cmd+shift+l" } else { "ctrl+shift+l" };
        assert_eq!(keymap.chord(Action::Leave), Some(chord(leave)));
    }

    #[test]
    fn ids_round_trip() {
        for action in Action::ALL {
            assert_eq!(Action::from_id(action.id()), Some(action));
        }
    }

    #[test]
    fn a_taken_key_is_refused_by_name() {
        let mut keymap = Keymap::default();
        assert_eq!(keymap.set(Action::ToggleMic, chord("v")), Err(Refusal::Taken(Action::ToggleCamera)));
        assert_eq!(keymap.chord(Action::ToggleMic), Some(chord("m")), "unchanged");
        assert_eq!(keymap.set(Action::ToggleMic, chord("esc")), Err(Refusal::Reserved));
    }

    #[test]
    fn set_unbind_and_reset() {
        let mut keymap = Keymap::default();
        assert_eq!(keymap.set(Action::ToggleMic, chord("k")), Ok(()));
        assert_eq!(keymap.lookup(chord("k")), Some(Action::ToggleMic));
        assert_eq!(keymap.lookup(chord("m")), None);
        assert!(!keymap.is_default(Action::ToggleMic));

        keymap.unbind(Action::ToggleCamera);
        assert_eq!(keymap.chord(Action::ToggleCamera), None);
        assert_eq!(keymap.lookup(chord("v")), None);
        // A freed default can be taken.
        assert_eq!(keymap.set(Action::ToggleMic, chord("v")), Ok(()));
        // And then its owner cannot reset onto it.
        assert_eq!(keymap.reset(Action::ToggleCamera), Err(Refusal::Taken(Action::ToggleMic)));

        assert_eq!(keymap.reset(Action::ToggleMic), Ok(()));
        assert!(keymap.is_default(Action::ToggleMic));
        keymap.reset_all();
        assert_eq!(keymap, Keymap::default());
    }

    #[test]
    fn setting_the_default_is_not_an_override() {
        let mut keymap = Keymap::default();
        keymap.set(Action::ToggleMic, chord("k")).unwrap();
        keymap.set(Action::ToggleMic, chord("m")).unwrap();
        assert!(keymap.overrides().is_empty());
    }

    #[test]
    fn overrides_round_trip_through_their_saved_form() {
        let mut keymap = Keymap::default();
        keymap.set(Action::ToggleMic, chord("ctrl+alt+m")).unwrap();
        keymap.unbind(Action::ShareScreen);
        let saved = keymap.overrides();
        assert_eq!(saved.get("toggle_mic").map(String::as_str), Some("ctrl+alt+m"));
        assert_eq!(saved.get("share_screen").map(String::as_str), Some(""));
        assert_eq!(saved.len(), 2, "only what changed");
        assert_eq!(Keymap::from_overrides(&saved), keymap);
    }

    #[test]
    fn a_bad_file_falls_back_to_defaults() {
        let saved: BTreeMap<String, String> = [
            ("toggle_mic", "hyper+m"),
            ("teleport", "t"),
            ("toggle_camera", "esc"),
            ("share_screen", "x"),
        ]
        .into_iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
        let keymap = Keymap::from_overrides(&saved);
        assert_eq!(keymap.chord(Action::ToggleMic), Some(chord("m")));
        assert_eq!(keymap.chord(Action::ToggleCamera), Some(chord("v")));
        assert_eq!(keymap.chord(Action::ShareScreen), Some(chord("x")));
    }

    #[test]
    fn a_hand_made_clash_loads_and_the_first_action_wins() {
        let saved: BTreeMap<String, String> = [("toggle_camera".to_string(), "m".to_string())].into_iter().collect();
        let keymap = Keymap::from_overrides(&saved);
        assert_eq!(keymap.lookup(chord("m")), Some(Action::ToggleMic));
    }
}

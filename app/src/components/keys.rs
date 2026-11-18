//! Keyboard shortcuts on the page: getting key presses at all, and turning
//! them into [`Chord`]s for [`crate::hotkeys`].
//!
//! **A key press only reaches us while something of ours has focus.** Blitz
//! sends a key to the focused element, and with nothing focused to the
//! document's root -- `<html>`, above every handler we can attach. So the
//! page's own root element takes focus when it mounts, and takes it back
//! whenever it loses it: a click on anything blitz does not treat as
//! focusable (all our buttons) clears focus, and a click on a video tile moves
//! it to the tile. A focused text field is left alone, and shortcuts pause
//! while it has focus.
//!
//! **Focus is only ever moved from an event handler.** Blitz's document sits
//! in a `RefCell` that Dioxus holds while it renders and runs effects and
//! spawned tasks, and focusing borrows it: the first version focused from a
//! spawned task and panicked at startup ("RefCell already borrowed"). An
//! event handler runs with the document free. The blur that follows a click
//! is such a handler, and it comes after blitz has cleared the focus, which
//! is why the root listens for its own blur rather than for clicks alone.
//!
//! In-app only, on purpose: no global key tap, so no new OS permission, and
//! nothing fires while another app is in front.

use std::rc::Rc;

use dioxus_native::prelude::*;

use super::settings::Tab;
use crate::hotkeys::{Action, Chord, Key as HotKey, Keymap, Mods, Named, Refusal};
use crate::settings::Settings;

/// The page's root element, and whether a text field has the keyboard.
#[derive(Clone, Copy)]
pub struct KeyFocus {
    root: Signal<Option<Rc<MountedData>>>,
    /// Set while a text field is focused: shortcuts pause, and focus is not
    /// taken from it.
    pub typing: Signal<bool>,
    /// Tab was pressed: the blur that follows is keyboard navigation, and
    /// focus is left where Tab put it. Keys still reach us from there, since
    /// they bubble up through the root.
    tabbing: Signal<bool>,
}

impl KeyFocus {
    /// Records the root element once it is mounted, and focuses it. Called
    /// from `onmounted`, which runs with the document free.
    pub fn mounted(mut self, root: Rc<MountedData>) {
        self.root.set(Some(root));
        self.restore();
    }

    /// Gives the keyboard back to the root element, unless someone is typing.
    /// **From an event handler only**; see the module docs.
    pub fn restore(self) {
        if *self.typing.peek() {
            return;
        }
        let root = self.root.peek().clone();
        if let Some(root) = root {
            // The work is done by the call; the future it returns is already
            // finished and carries nothing worth waiting for.
            drop(root.set_focus(true));
        }
    }

    /// The root element lost focus: take it back, unless Tab moved it on.
    pub fn root_blurred(mut self) {
        if *self.tabbing.peek() {
            self.tabbing.set(false);
            return;
        }
        self.restore();
    }

    /// Notes whether the key just pressed was Tab, for [`root_blurred`](Self::root_blurred).
    pub fn key_pressed(mut self, tab: bool) {
        if *self.tabbing.peek() != tab {
            self.tabbing.set(tab);
        }
    }

    /// A text field gained or lost the keyboard. From its event handlers.
    pub fn set_typing(mut self, typing: bool) {
        if *self.typing.peek() != typing {
            self.typing.set(typing);
        }
        if !typing {
            self.restore();
        }
    }

    /// The text field went away without a blur. Only notes it: this is called
    /// from an effect, where focus cannot be moved, so the root takes the
    /// keyboard back at the next click.
    pub fn typing_gone(mut self) {
        if *self.typing.peek() {
            self.typing.set(false);
        }
    }
}

/// The current key bindings, shared by the page's key handler and the
/// settings' keyboard tab.
#[derive(Clone, Copy)]
pub struct Hotkeys(pub Signal<Keymap>);

impl Hotkeys {
    /// Changes the bindings and saves them, unless the change is refused;
    /// a refused change leaves everything as it was. Takes effect at once.
    pub fn change(self, change: impl FnOnce(&mut Keymap) -> Result<(), Refusal>) -> Result<(), Refusal> {
        let Hotkeys(mut keymap) = self;
        let mut next = keymap.peek().clone();
        change(&mut next)?;
        let saved = next.overrides();
        keymap.set(next);
        Settings::update(move |settings| settings.hotkeys = saved);
        Ok(())
    }
}

/// Makes [`KeyFocus`] and [`Hotkeys`] available below, the latter loaded from
/// the settings file.
pub fn use_keys_provider() -> (KeyFocus, Hotkeys) {
    let focus = use_context_provider(|| KeyFocus {
        root: Signal::new(None),
        typing: Signal::new(false),
        tabbing: Signal::new(false),
    });
    let hotkeys = use_context_provider(|| Hotkeys(Signal::new(Keymap::from_overrides(&Settings::load().hotkeys))));
    (focus, hotkeys)
}

/// The chord a key press is, or `None` for a press that is only a modifier,
/// or a key no chord can name.
///
/// The thin end of the shortcuts: everything after this is
/// [`crate::hotkeys`], which knows nothing of Dioxus.
pub fn chord_of(event: &KeyboardEvent) -> Option<Chord> {
    let modifiers = event.modifiers();
    let mods = Mods {
        cmd: modifiers.contains(Modifiers::META) || modifiers.contains(Modifiers::SUPER),
        ctrl: modifiers.contains(Modifiers::CONTROL),
        alt: modifiers.contains(Modifiers::ALT),
        shift: modifiers.contains(Modifiers::SHIFT),
    };
    let key = match event.key() {
        // On macOS Option composes a different character (Option+L is ¬), so
        // with Alt held a letter or digit is read from the physical key.
        Key::Character(_) if mods.alt => physical(event.code()).or_else(|| character(&event.key())),
        Key::Character(_) => character(&event.key()),
        Key::Enter => Some(HotKey::Named(Named::Enter)),
        Key::Tab => Some(HotKey::Named(Named::Tab)),
        Key::Backspace => Some(HotKey::Named(Named::Backspace)),
        Key::Delete => Some(HotKey::Named(Named::Delete)),
        Key::Escape => Some(HotKey::Named(Named::Escape)),
        Key::ArrowUp => Some(HotKey::Named(Named::Up)),
        Key::ArrowDown => Some(HotKey::Named(Named::Down)),
        Key::ArrowLeft => Some(HotKey::Named(Named::Left)),
        Key::ArrowRight => Some(HotKey::Named(Named::Right)),
        Key::Home => Some(HotKey::Named(Named::Home)),
        Key::End => Some(HotKey::Named(Named::End)),
        Key::PageUp => Some(HotKey::Named(Named::PageUp)),
        Key::PageDown => Some(HotKey::Named(Named::PageDown)),
        Key::Insert => Some(HotKey::Named(Named::Insert)),
        Key::F1 => Some(HotKey::F(1)),
        Key::F2 => Some(HotKey::F(2)),
        Key::F3 => Some(HotKey::F(3)),
        Key::F4 => Some(HotKey::F(4)),
        Key::F5 => Some(HotKey::F(5)),
        Key::F6 => Some(HotKey::F(6)),
        Key::F7 => Some(HotKey::F(7)),
        Key::F8 => Some(HotKey::F(8)),
        Key::F9 => Some(HotKey::F(9)),
        Key::F10 => Some(HotKey::F(10)),
        Key::F11 => Some(HotKey::F(11)),
        Key::F12 => Some(HotKey::F(12)),
        // Shift, Control, Meta and the rest: a modifier alone is not a press.
        _ => None,
    }?;
    Some(Chord::pressed(mods, key))
}

/// A one-character key as a chord's key.
fn character(key: &Key) -> Option<HotKey> {
    let Key::Character(text) = key else { return None };
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => HotKey::from_char(c),
        _ => None,
    }
}

/// A letter or digit by its physical position.
fn physical(code: Code) -> Option<HotKey> {
    let name = code.to_string();
    let rest = name.strip_prefix("Key").or_else(|| name.strip_prefix("Digit"))?;
    let mut chars = rest.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => HotKey::from_char(c),
        _ => None,
    }
}

/// The settings window: whether it is open, and the shortcut being recorded.
#[derive(Clone, Copy)]
pub struct SettingsPanel {
    pub open: Signal<bool>,
    /// The action whose new key the next press becomes.
    pub recording: Signal<Option<Action>>,
    /// A refused key, shown under the row it was meant for.
    pub notice: Signal<Option<(Action, String)>>,
    /// The tab showing. Here rather than in the window, so something outside
    /// it (the update banner) can open it on a given tab.
    pub tab: Signal<Tab>,
}

impl SettingsPanel {
    pub fn show(mut self) {
        self.open.set(true);
    }

    /// Opens it on `tab`.
    pub fn show_tab(mut self, tab: Tab) {
        if *self.tab.peek() != tab {
            self.tab.set(tab);
        }
        self.show();
    }

    /// Closes it, abandoning any recording, and gives the keyboard back to
    /// the page.
    ///
    /// **Even from one of its text fields.** Hidden with the window, a field
    /// that has the keyboard sends no blur, so `typing` would stay set and the
    /// shortcuts paused until the next click; and `restore` declines while it
    /// is set. So this clears it, which also restores.
    pub fn close(mut self, focus: KeyFocus) {
        self.open.set(false);
        self.recording.set(None);
        self.notice.set(None);
        focus.set_typing(false);
    }
}

/// Makes the shared [`SettingsPanel`] available below.
pub fn use_settings_panel_provider() -> SettingsPanel {
    use_context_provider(|| SettingsPanel {
        open: Signal::new(false),
        recording: Signal::new(None),
        notice: Signal::new(None),
        tab: Signal::new(Tab::Profile),
    })
}

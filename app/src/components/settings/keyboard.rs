//! The Keyboard tab: every shortcut, its key, and changing it.
//!
//! Clicking a key cap records: the next key pressed becomes the binding, Esc
//! gives up, and Backspace or Delete leaves the action without a key. The
//! press itself arrives through the page's key handler (`call_screen`),
//! which hands it to [`record`] before anything else sees it.

use dioxus_native::prelude::*;

use super::super::keys::{Hotkeys, SettingsPanel};
use super::{Heading, Row};
use crate::hotkeys::{Action, Chord, Key, Mods, Named, Refusal, Section};

/// Key caps use the platform's own notation: ⇧⌘L on a Mac, Ctrl+Shift+L
/// elsewhere.
const MAC: bool = cfg!(target_os = "macos");

/// Takes a key press as `action`'s new key, while the tab is recording.
pub fn record(panel: SettingsPanel, hotkeys: Hotkeys, action: Action, chord: Chord) {
    let SettingsPanel {
        mut recording,
        mut notice,
        ..
    } = panel;
    recording.set(None);
    notice.set(None);
    if chord.is_escape() {
        return;
    }
    let result = if chord.mods == Mods::NONE
        && matches!(chord.key, Key::Named(Named::Backspace) | Key::Named(Named::Delete))
    {
        hotkeys.change(|keymap| {
            keymap.unbind(action);
            Ok(())
        })
    } else {
        hotkeys.change(|keymap| keymap.set(action, chord))
    };
    match result {
        Ok(()) => tracing::info!("shortcut for {} is now {}", action.id(), describe(hotkeys, action)),
        Err(refusal) => notice.set(Some((action, refused(refusal)))),
    }
}

/// A refusal in words, for the row it was meant for.
fn refused(refusal: Refusal) -> String {
    match refusal {
        Refusal::Taken(other) => format!("Already used by {}", other.title()),
        Refusal::Reserved => "Esc closes things, so it cannot be a shortcut".to_string(),
    }
}

/// The action's key as `settings.json` would write it, for the log.
fn describe(hotkeys: Hotkeys, action: Action) -> String {
    let Hotkeys(keymap) = hotkeys;
    let chord = keymap.peek().chord(action);
    chord.map_or_else(|| "unbound".to_string(), |chord| chord.to_string())
}

#[component]
pub fn KeyboardTab() -> Element {
    let panel = use_context::<SettingsPanel>();
    let hotkeys = use_context::<Hotkeys>();
    let Hotkeys(keymap) = hotkeys;
    let SettingsPanel {
        mut recording,
        mut notice,
        ..
    } = panel;
    let keymap = keymap();
    let recording_now = recording();
    let notice_now = notice();
    let changed = Action::ALL.iter().any(|action| !keymap.is_default(*action));

    rsx! {
        div { class: "flex flex-col",
            div { class: "text-lg font-semibold text-neutral-100", "Keyboard" }
            p { class: "mt-1 text-sm text-neutral-400",
                "Shortcuts work while the vdr window is in front, and pause while you type in a text field. Click a key to change it."
            }
            for section in Section::ALL {
                Heading { text: section.title() }
                div { class: "flex flex-col",
                    for (index, action) in Action::ALL.into_iter().filter(|action| action.section() == section).enumerate() {
                        Row {
                            key: "{action.id()}",
                            title: action.title(),
                            description: action.description(),
                            first: index == 0,
                            notice: notice_now.as_ref().filter(|(at, _)| *at == action).map(|(_, text)| text.clone()),
                            if !keymap.is_default(action) {
                                button {
                                    class: "shrink-0 whitespace-nowrap rounded px-2 py-1 text-xs text-neutral-400 hover:bg-neutral-800",
                                    onclick: move |_| {
                                        recording.set(None);
                                        notice.set(None);
                                        if let Err(refusal) = hotkeys.change(|keymap| keymap.reset(action)) {
                                            let default = action.default_chord().label(MAC);
                                            let why = match refusal {
                                                Refusal::Taken(other) => format!("{default} is used by {}", other.title()),
                                                Refusal::Reserved => refused(refusal),
                                            };
                                            notice.set(Some((action, why)));
                                        }
                                    },
                                    "Reset"
                                }
                            }
                            KeyCap {
                                chord: keymap.chord(action),
                                recording: recording_now == Some(action),
                                onclick: move |_| {
                                    notice.set(None);
                                    let next = if *recording.peek() == Some(action) { None } else { Some(action) };
                                    recording.set(next);
                                },
                            }
                        }
                    }
                }
            }
            div { class: "mt-6 flex flex-row",
                button {
                    class: if changed {
                        "shrink-0 whitespace-nowrap rounded-md bg-neutral-800 px-3 py-1.5 text-xs text-neutral-100 hover:bg-neutral-700"
                    } else {
                        "shrink-0 whitespace-nowrap rounded-md bg-neutral-800 px-3 py-1.5 text-xs text-neutral-500"
                    },
                    onclick: move |_| {
                        recording.set(None);
                        notice.set(None);
                        let _ = hotkeys.change(|keymap| {
                            keymap.reset_all();
                            Ok(())
                        });
                        tracing::info!("shortcuts reset to their defaults");
                    },
                    "Reset all to defaults"
                }
            }
        }
    }
}

/// A binding drawn as keys: one pill per key to press (⇧ ⌘ L), or a single
/// pill for "None" and while recording. The whole row is the click target.
///
/// One pill per key rather than one string: in a single run of text the
/// macOS modifier glyphs came out small and jammed against the letter.
#[component]
fn KeyCap(chord: Option<Chord>, recording: bool, onclick: EventHandler<MouseEvent>) -> Element {
    rsx! {
        div {
            class: "flex cursor-pointer flex-row items-center gap-1",
            onclick: move |event| onclick.call(event),
            if recording {
                div { class: "flex h-7 items-center whitespace-nowrap rounded-md border border-blue-500 bg-neutral-800 px-2.5 text-xs text-blue-300",
                    "Press a key\u{2026}"
                }
            } else if let Some(chord) = chord {
                for (index, cap) in chord.caps(MAC).into_iter().enumerate() {
                    div {
                        key: "{index}",
                        // The modifier glyphs a size up, to sit level with a
                        // capital letter.
                        class: if is_symbol(&cap) {
                            "flex h-7 min-w-7 items-center justify-center whitespace-nowrap rounded-md border border-neutral-700 bg-neutral-800 px-1.5 text-base text-neutral-100"
                        } else {
                            "flex h-7 min-w-7 items-center justify-center whitespace-nowrap rounded-md border border-neutral-700 bg-neutral-800 px-1.5 text-xs text-neutral-100"
                        },
                        "{cap}"
                    }
                }
            } else {
                div { class: "flex h-7 items-center whitespace-nowrap rounded-md border border-neutral-800 px-2.5 text-xs text-neutral-500",
                    "None"
                }
            }
        }
    }
}

/// Whether a cap is one of the macOS modifier glyphs.
fn is_symbol(cap: &str) -> bool {
    matches!(cap, "\u{2303}" | "\u{2325}" | "\u{21E7}" | "\u{2318}")
}

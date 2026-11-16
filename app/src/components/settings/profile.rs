//! The Profile tab: what everyone in a call sees you called.
//!
//! One field, applied as you type: each keystroke is cleaned into our own
//! entry, and the presence announcer carries it to the room within a quarter
//! second, mid-call included, with no rejoin. The settings file is written
//! when the field lets go of the keyboard, on Enter, when the window closes,
//! and when the tab goes -- not on every key, since each save is a
//! read-modify-write on a thread of its own and two in flight could land out
//! of order.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use dioxus_native::prelude::*;

use super::super::keys::{KeyFocus, SettingsPanel};
use super::Row;
use crate::name::clean;
use crate::net::PeerStats;
use crate::settings::Settings;

#[component]
pub fn ProfileTab(local: Arc<PeerStats>) -> Element {
    let focus = use_context::<KeyFocus>();
    let panel = use_context::<SettingsPanel>();
    let SettingsPanel { mut recording, .. } = panel;
    // **The text as typed**, not as cleaned: cleaning in place would eat the
    // space between two words the moment it was typed.
    let mut draft = use_signal({
        let local = local.clone();
        move || local.name().unwrap_or_default()
    });

    // Typed since the last save. Not a signal: the drop below reads it, and
    // a signal may already be gone by then. Mounted with the window, so
    // without it every launch would write the file for nothing.
    let unsaved = use_hook(|| Rc::new(Cell::new(false)));

    // Closed while the field had the keyboard: no blur comes, so this is
    // what saves.
    use_effect({
        let local = local.clone();
        let unsaved = unsaved.clone();
        move || {
            if !(panel.open)() {
                save(&local, &unsaved);
            }
        }
    });
    use_drop({
        let local = local.clone();
        let unsaved = unsaved.clone();
        move || save(&local, &unsaved)
    });

    // Saved, and the field shows what the room sees.
    let settle = {
        let local = local.clone();
        let unsaved = unsaved.clone();
        move || {
            save(&local, &unsaved);
            let cleaned = local.name().unwrap_or_default();
            if *draft.peek() != cleaned {
                draft.set(cleaned);
            }
        }
    };

    rsx! {
        div { class: "flex flex-col",
            div { class: "text-lg font-semibold text-neutral-100", "Profile" }
            p { class: "mt-1 text-sm text-neutral-400",
                "Names are not verified: anyone can choose any name, and two people can choose the same one. A person's tile menu also shows their short id, to tell them apart."
            }
            div { class: "mt-6 flex flex-col",
                Row {
                    title: "Display name",
                    description: "Shown under your video to everyone in the call",
                    first: true,
                    input {
                        class: "w-64 rounded-md bg-neutral-800 px-3 py-2 text-sm text-neutral-100",
                        r#type: "text",
                        placeholder: "Your name",
                        value: "{draft}",
                        // Shortcuts pause while this has the keyboard, as for
                        // the room-key field: `m` here is a letter of a name.
                        // Noted on the press too, before blitz moves focus
                        // here, so the page's root does not take it straight
                        // back. A shortcut being recorded is given up, or the
                        // first letter typed would become its key.
                        onmousedown: move |_| {
                            if recording.peek().is_some() {
                                recording.set(None);
                            }
                            focus.set_typing(true);
                        },
                        onfocus: move |_| focus.set_typing(true),
                        onblur: {
                            let mut settle = settle.clone();
                            move |_| {
                                settle();
                                focus.set_typing(false);
                            }
                        },
                        oninput: {
                            let local = local.clone();
                            move |evt: FormEvent| {
                                let typed = evt.value();
                                local.set_name(clean(&typed));
                                unsaved.set(true);
                                draft.set(typed);
                            }
                        },
                        onkeydown: {
                            let mut settle = settle.clone();
                            move |evt: KeyboardEvent| {
                                // Enter arrives as a named key or as a newline
                                // character depending on the platform's IME path.
                                let is_enter = matches!(evt.key(), Key::Enter)
                                    || matches!(evt.key(), Key::Character(ref s) if s == "\n");
                                if is_enter {
                                    evt.prevent_default();
                                    settle();
                                }
                            }
                        },
                    }
                }
            }
        }
    }
}

/// Writes our name as it stands to the settings file, already cleaned, if
/// it has been typed since the last write.
fn save(local: &PeerStats, unsaved: &Cell<bool>) {
    if !unsaved.replace(false) {
        return;
    }
    let name = local.name();
    Settings::update(move |settings| settings.name = name);
}

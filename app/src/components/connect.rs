//! Choosing who to call, without the command line.
//!
//! **Inline, never an overlay.** Absolutely-positioned content paints but never
//! receives clicks in this blitz revision — `Node::hit` will not descend into a
//! child overflowing its parent's box — so a modal or popover here would look
//! right and be dead to the mouse. Ticket 0 tracks the renderer fix; until then
//! this is a row in normal flow, the same shape `dropdown.rs` was forced into.
//!
//! This does not replace `--dial` / `--listen`. Those are how two instances get
//! driven from a script, which is how the network path is tested; see
//! [`crate::args`].

use dioxus_native::prelude::*;
use iroh::EndpointId;

use super::{Button, Size, Tone};
use crate::args::Peering;
use crate::net::RemoteStatus;

/// The idle controls: our key to hand out, and a field to take theirs.
///
/// Renders nothing once a decision has been made — from then on the status line
/// says everything there is to say.
#[component]
pub fn ConnectRow(
    /// Our own endpoint id, once the endpoint is bound.
    me: Signal<Option<String>>,
    /// Reported into on a bad key, and read to decide whether to render.
    status: Signal<RemoteStatus>,
    /// Chooses who to talk to.
    connect: Callback<Peering>,
    /// Takes a choice back while it is still only a choice.
    cancel: Callback<()>,
) -> Element {
    let mut draft = use_signal(String::new);
    let mut status = status;

    // Declared before the early return below: hooks must run in the same order
    // on every render, so none of them may sit behind a conditional.
    let dial = use_callback(move |()| {
        let typed = draft.read().trim().to_string();
        if typed.is_empty() {
            return;
        }
        match typed.parse::<EndpointId>() {
            Ok(peer) => connect.call(Peering::Dial(peer)),
            // Reported in place rather than refused silently: a key that does
            // not parse is nearly always a partial paste, and saying so is the
            // difference between a typo and a mystery.
            Err(error) => status.set(RemoteStatus::BadKey {
                reason: error.to_string(),
            }),
        }
    });

    let waiting = status.read().is_waiting();
    let cancellable = status.read().is_cancellable();
    // Neither choosing nor able to unchoose: a call is up, or it ended. The
    // status line says the rest.
    //
    // The two together span every state from binding to connected, so the key
    // and its Copy button stay mounted the whole way. They used to drop out
    // during `AwaitingCamera` -- the second or so between pressing a button and
    // the encoder being ready -- which read as a flash.
    if !waiting && !cancellable {
        return rsx! {};
    }

    rsx! {
        div { class: "flex flex-col gap-2",
            // Shown while waiting for a call as well as while idle: that is
            // precisely when the key has to be handed to someone, and it used
            // to vanish at exactly that moment.
            div { class: "flex flex-row items-center gap-3",
                span { class: "text-sm text-neutral-400", "Your key" }
                code { class: "text-xs text-neutral-200", {me.read().clone().unwrap_or_else(|| "binding...".to_string())} }
                if me.read().is_some() {
                    Button {
                        size: Size::Small,
                        onclick: move |_| {
                            let Some(key) = me.read().clone() else { return };
                            copy_to_clipboard(&key);
                        },
                        "Copy"
                    }
                }
            }
            if cancellable {
                div { class: "flex flex-row items-center gap-3",
                    Button {
                        onclick: move |_| cancel.call(()),
                        "Cancel"
                    }
                }
            }
            if waiting {
            div { class: "flex flex-row items-center gap-3",
                input {
                    class: "w-96 rounded-md bg-neutral-900 px-3 py-2 text-sm text-neutral-100",
                    r#type: "text",
                    placeholder: "Paste the other person's key",
                    value: "{draft}",
                    oninput: move |evt| {
                        // Clears a previous complaint as soon as the text
                        // changes, so the error belongs to what is on screen.
                        if status.peek().is_error() {
                            if let Some(me) = me.peek().clone() {
                                status.set(RemoteStatus::Idle { me });
                            }
                        }
                        draft.set(evt.value());
                    },
                    onkeydown: move |evt: KeyboardEvent| {
                        // Enter arrives as a named key or as a newline
                        // character depending on the platform's IME path.
                        let is_enter = matches!(evt.key(), Key::Enter)
                            || matches!(evt.key(), Key::Character(ref s) if s == "\n");
                        if is_enter {
                            dial.call(());
                            evt.prevent_default();
                        }
                    },
                }
                Button {
                    tone: Tone::Primary,
                    onclick: move |_| dial.call(()),
                    "Connect"
                }
                Button {
                    onclick: move |_| connect.call(Peering::Listen),
                    "Wait for a call"
                }
            }
            }
        }
    }
}

/// Puts `text` on the system clipboard.
///
/// Direct `arboard` rather than anything from dioxus-native: its
/// `convert_clipboard_data` is `unimplemented!()`, and blitz's own clipboard
/// access is private to its keyboard handler (which is what makes Cmd-V in the
/// field above work). `arboard` is already compiled into the tree underneath
/// blitz-shell, so asking for it directly costs no packages.
///
/// Failing to copy is a log line: it is a convenience over text already on
/// screen, and there is nothing useful to tell the user.
fn copy_to_clipboard(text: &str) {
    match arboard::Clipboard::new() {
        Ok(mut clipboard) => {
            if let Err(error) = clipboard.set_text(text.to_owned()) {
                tracing::warn!("could not write to the clipboard: {error}");
            }
        }
        Err(error) => tracing::warn!("no clipboard available: {error}"),
    }
}

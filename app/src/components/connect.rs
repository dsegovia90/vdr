//! Choosing who to call, without the command line.
//!
//! **A row in normal flow.** It was made inline because an older blitz would
//! not deliver clicks to absolutely-positioned content. The pinned tag does, so
//! an overlay would work here now; the row simply has not needed to change.
//!
//! This does not replace `--dial` / `--listen`. Those are how two instances get
//! driven from a script, which is how the network path is tested; see
//! [`crate::args`].

use dioxus_native::prelude::*;
use iroh::EndpointId;

use super::{Button, KeyFocus, Size, Tone};
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
        // A URL is a room on a relay; anything else has to be someone's key.
        // Told apart by `://`, which no endpoint id contains, so a mistyped
        // URL gets the URL's complaint rather than the key's.
        if typed.contains("://") {
            join_room(&typed, connect, status);
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

    // **The field going away while it has the keyboard** sends no blur:
    // blitz just drops the focus. Enter, which dials, gives the keyboard back
    // itself; this catches anything else that ends the waiting, so shortcuts
    // do not stay paused.
    let focus = use_context::<KeyFocus>();
    use_effect(move || {
        if !status.read().is_waiting() {
            focus.typing_gone();
        }
    });

    let waiting = status.read().is_waiting();
    let cancellable = status.read().is_cancellable();
    let connected = status.read().is_connected();
    // Gone once a call or room is actually up: from then on the screen is the
    // grid and the bottom bar, which carries Leave. Until then -- including
    // while waiting for a call, which is exactly when the key has to be handed
    // over -- it stays, so the key never flashes out of view between pressing
    // a button and the encoder being ready.
    if connected || (!waiting && !cancellable) {
        return rsx! {};
    }

    rsx! {
        div { class: "flex flex-col gap-2",
            // Shown while waiting for a call as well as while idle: that is
            // precisely when the key has to be handed to someone, and it used
            // to vanish at exactly that moment.
            div { class: "flex flex-row items-center gap-3",
                span { class: "text-sm text-neutral-400 whitespace-nowrap", "Your key" }
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
            if waiting {
            div { class: "flex flex-row items-center gap-3",
                input {
                    class: "w-96 rounded-md bg-neutral-900 px-3 py-2 text-sm text-neutral-100",
                    r#type: "text",
                    placeholder: "Paste the other person's key, or a relay room URL",
                    value: "{draft}",
                    // Shortcuts pause while this has the keyboard: `m` here is
                    // a letter of someone's key, not the microphone. Noted on
                    // the press too, before blitz moves focus here, so the
                    // page's root does not take it straight back.
                    onmousedown: move |_| focus.set_typing(true),
                    onfocus: move |_| focus.set_typing(true),
                    onblur: move |_| focus.set_typing(false),
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
                            // Dialled: this field is about to go, and with it
                            // the focus. A key that did not parse leaves it.
                            if !status.peek().is_waiting() {
                                focus.set_typing(false);
                            }
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

/// Joins the room a URL names, or says why it is not one.
///
/// The field's Connect and a recent room's Join both come through here, so a
/// remembered room fails, shows progress and cancels exactly as a pasted one.
pub(super) fn join_room(text: &str, connect: Callback<Peering>, mut status: Signal<RemoteStatus>) {
    match crate::args::relay_url(text) {
        Ok(url) => connect.call(Peering::Join(url)),
        Err(reason) => status.set(RemoteStatus::BadKey { reason }),
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
pub(super) fn copy_to_clipboard(text: &str) {
    match arboard::Clipboard::new() {
        Ok(mut clipboard) => {
            if let Err(error) = clipboard.set_text(text.to_owned()) {
                tracing::warn!("could not write to the clipboard: {error}");
            }
        }
        Err(error) => tracing::warn!("no clipboard available: {error}"),
    }
}

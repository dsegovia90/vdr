//! The Experimental tab: a switch per feature still being tried out.
//!
//! Each feature has its own switch and its own setting, named for what it
//! does, so one can graduate or go away without touching the others. Each
//! says whether it is on by default; a person changes it for themselves.
//!
//! The connection report (on by default): our presence tells the room how
//! our link is doing and what we get from each person, and the tiles and the
//! Network tab show what everyone else says.
//!
//! Hosting the rooms we join (**off** by default, because it lends our
//! uplink to other people's video): while in a room, our app also accepts its
//! members directly and says so, and their apps keep that with the room, so
//! it can still meet when its relay is down. A join reads the switch as it
//! starts, so turning it on waits for the next one; turning it off stops
//! serving at once.
//!
//! Moving the call (**off** by default, because a move sends everyone's
//! video through someone's computer): we follow where the room says the call
//! is, and can move it to a host from that person's tile menu. Off, we
//! neither follow nor say, and our media goes back to the usual order.
//!
//! **Off means neither publish nor show.** The flag lives on our own entry,
//! where the presence announcer reads it four times a second (a report
//! already out is withdrawn with the next document), and the tiles and the
//! Network tab read it the same way. Nothing here touches the call.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use dioxus_native::prelude::*;

use super::Row;
use super::updates::Switch;
use crate::channel::use_channel;
use super::super::{Button, Size};
use crate::net::PeerStats;
use crate::settings::Settings;

/// How often the tab looks at where the call is: a move arrives from the
/// network, not from a click here.
const POLL: Duration = Duration::from_secs(1);

/// `me`: our endpoint id, which a move we make is said under.
#[component]
pub fn ExperimentalTab(local: Arc<PeerStats>, me: String) -> Element {
    let mut report = use_signal({
        let local = local.clone();
        move || local.connection_report()
    });
    let mut hosting = use_signal({
        let local = local.clone();
        move || local.host_rooms()
    });
    let hosts = local.clone();
    let mut moving = use_signal({
        let local = local.clone();
        move || local.move_calls()
    });
    let mut call_on = use_signal({
        let local = local.clone();
        move || local.call_host()
    });
    // Sampled off the page, and sent only on a change: any message wakes the
    // page, and a woken page repaints the whole window.
    let stop = use_hook(|| Arc::new(AtomicBool::new(false)));
    use_drop({
        let stop = stop.clone();
        move || stop.store(true, Ordering::Relaxed)
    });
    let tx = use_channel(move |next: Option<String>| call_on.set(next));
    use_hook({
        let local = local.clone();
        let stop = stop.clone();
        let first = call_on.peek().clone();
        move || {
            let spawned = std::thread::Builder::new().name("experimental-tab".into()).spawn(move || {
                let mut sent = first;
                loop {
                    std::thread::sleep(POLL);
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    let next = local.call_host();
                    if next != sent {
                        if tx.unbounded_send(next.clone()).is_err() {
                            return;
                        }
                        sent = next;
                    }
                }
            });
            if let Err(error) = spawned {
                tracing::warn!("could not spawn the experimental tab's poller: {error}");
            }
        }
    });
    let where_now = match call_on() {
        None => "On the relay, or wherever the usual order puts it when the relay is down.".to_string(),
        Some(id) => format!("On host {}. Everyone following moves sends their video through that computer.", crate::name::short(&id)),
    };
    let movers = local.clone();
    let back = local.clone();
    rsx! {
        div { class: "flex flex-col",
            div { class: "text-lg font-semibold text-neutral-100", "Experimental" }
            p { class: "mt-1 text-sm text-neutral-400",
                "Features still being tried out. They may change or go away."
            }
            div { class: "mt-6 flex flex-col",
                Row {
                    title: "Connection report",
                    description: "Tells everyone in the call how your connection is doing and shows how theirs is, as bars on each video and in the Network tab. Turning it off stops telling the room about your connection and hides theirs.",
                    first: true,
                    Switch {
                        on: report(),
                        onclick: move |_| {
                            let next = !*report.peek();
                            report.set(next);
                            local.set_connection_report(next);
                            tracing::info!("connection report {}", if next { "on" } else { "off" });
                            Settings::update(move |settings| settings.connection_report = Some(next));
                        },
                    }
                }
                Row {
                    title: "Host rooms I'm in",
                    first: false,
                    description: "While you are in a room, its members can also connect through your computer, and their apps remember how, so the room can still meet if its relay is down. Their video may then travel over your connection and use your upload. Takes effect the next time you join a room.",
                    Switch {
                        on: hosting(),
                        onclick: move |_| {
                            let next = !*hosting.peek();
                            hosting.set(next);
                            hosts.set_host_rooms(next);
                            tracing::info!("hosting rooms {}", if next { "on" } else { "off" });
                            Settings::update(move |settings| settings.host_rooms = Some(next));
                        },
                    }
                }
                Row {
                    title: "Move the call",
                    first: false,
                    description: "Follow the call when someone in the room moves it to a host, and move it yourself from a host's name on their video. For testing: while the call is on a host, everyone's video goes through that person's computer.",
                    Switch {
                        on: moving(),
                        onclick: move |_| {
                            let next = !*moving.peek();
                            moving.set(next);
                            movers.set_move_calls(next);
                            call_on.set(movers.call_host());
                            tracing::info!("moving the call {}", if next { "on" } else { "off" });
                            Settings::update(move |settings| settings.move_calls = Some(next));
                        },
                    }
                }
                if moving() {
                    Row {
                        title: "Where the call is",
                        first: false,
                        description: where_now,
                        Button {
                            size: Size::Small,
                            disabled: call_on().is_none() || me.is_empty(),
                            onclick: move |_| {
                                back.move_call(None, &me);
                                call_on.set(back.call_host());
                            },
                            "Back to automatic"
                        }
                    }
                }
            }
        }
    }
}

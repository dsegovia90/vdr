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
//! **Off means neither publish nor show.** The flag lives on our own entry,
//! where the presence announcer reads it four times a second (a report
//! already out is withdrawn with the next document), and the tiles and the
//! Network tab read it the same way. Nothing here touches the call.

use std::sync::Arc;

use dioxus_native::prelude::*;

use super::Row;
use super::updates::Switch;
use crate::net::PeerStats;
use crate::settings::Settings;

#[component]
pub fn ExperimentalTab(local: Arc<PeerStats>) -> Element {
    let mut report = use_signal({
        let local = local.clone();
        move || local.connection_report()
    });
    let mut hosting = use_signal({
        let local = local.clone();
        move || local.host_rooms()
    });
    let hosts = local.clone();
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
            }
        }
    }
}

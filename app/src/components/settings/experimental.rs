//! The Experimental tab: a switch per feature still being tried out.
//!
//! Each feature has its own switch and its own setting, named for what it
//! does, so one can graduate or go away without touching the others. Each is
//! on by default, for everyone; a person can turn one off for themselves.
//!
//! Today there is one, the connection report: our presence tells the room how
//! our link is doing and what we get from each person, and the tiles and the
//! Network tab show what everyone else says.
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
            }
        }
    }
}

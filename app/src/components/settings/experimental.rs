//! The Experimental tab: one switch for features still being tried out.
//!
//! On by default, for everyone; each person can turn it off for themselves.
//! Today it covers one thing, the connection report: our presence tells the
//! room how our link is doing and what we get from each person, and the tiles
//! and the Network tab show what everyone else says.
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
    let mut on = use_signal({
        let local = local.clone();
        move || local.experimental()
    });
    rsx! {
        div { class: "flex flex-col",
            div { class: "text-lg font-semibold text-neutral-100", "Experimental" }
            p { class: "mt-1 text-sm text-neutral-400",
                "Features still being tried out. They may change or go away."
            }
            div { class: "mt-6 flex flex-col",
                Row {
                    title: "Experimental features",
                    description: "Tells everyone in the call how your connection is doing and shows how theirs is, as bars on each video and in the Network tab. Turning it off stops telling the room about your connection and hides theirs.",
                    first: true,
                    Switch {
                        on: on(),
                        onclick: move |_| {
                            let next = !*on.peek();
                            on.set(next);
                            local.set_experimental(next);
                            tracing::info!("experimental features {}", if next { "on" } else { "off" });
                            Settings::update(move |settings| settings.experimental = Some(next));
                        },
                    }
                }
            }
        }
    }
}

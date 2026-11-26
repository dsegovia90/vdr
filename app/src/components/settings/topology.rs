//! The Topology tab: the room as a map of who links to whom (`room_map`),
//! with its legend, the switch for its motion and what the selected node
//! sends and receives.
//!
//! **Mounted only while it can be seen.** The window stays mounted while
//! closed, so the map is mounted only while `open` as well as while this tab
//! is the one shown: mounting it is what asks the room for everyone's routes
//! and starts its redraws, and dropping it is what stops both.

use std::sync::Arc;

use dioxus_native::prelude::*;

use super::room_map::RoomMap;
use crate::net::{Participant, PeerStats};

#[component]
pub fn TopologyTab(local: Arc<PeerStats>, participants: Signal<Vec<Participant>>, me: String, open: bool) -> Element {
    rsx! {
        div { class: "flex flex-col",
            div { class: "text-lg font-semibold text-neutral-100", "Topology" }
            // One line at the window's usual width: the square below is sized
            // to fit under it (see `room_map`), and the legend says what the
            // lines mean.
            p { class: "mt-1 text-sm text-neutral-400",
                "Everyone in the room and the relays it meets at, as each of them says it."
            }
            if open {
                RoomMap { local, participants, me }
            }
        }
    }
}

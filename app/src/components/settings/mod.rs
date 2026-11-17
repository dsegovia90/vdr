//! The settings window: a panel over the dimmed call, tabs down the left,
//! the chosen tab's rows on the right.
//!
//! **A new tab is a [`Tab`] variant and a component.** The rail, the
//! scrolling pane and the closing are the shell's; a tab only draws its rows,
//! with [`Heading`] and [`Row`] for the shared look.
//!
//! **Mounted always, hidden while closed**, as the bar's menus are: it has a
//! positive z-index, and blitz once panicked hit-testing such a node after it
//! was removed. It also keeps the open tab across a close.

mod about;
mod keyboard;
mod profile;

use std::sync::Arc;

use dioxus_native::prelude::*;

use super::keys::{KeyFocus, SettingsPanel};
use super::{CloseIcon, InfoIcon, KeyboardIcon, PersonIcon};
use crate::net::PeerStats;

pub(super) use keyboard::record;

/// One tab of the window.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Tab {
    Profile,
    Keyboard,
    About,
}

impl Tab {
    const ALL: [Tab; 3] = [Tab::Profile, Tab::Keyboard, Tab::About];

    fn label(self) -> &'static str {
        match self {
            Tab::Profile => "Profile",
            Tab::Keyboard => "Keyboard",
            Tab::About => "About",
        }
    }

    fn icon(self) -> Element {
        match self {
            // The avatar's figure at the rail's size.
            Tab::Profile => rsx! { PersonIcon { size: 16 } },
            Tab::Keyboard => rsx! { KeyboardIcon {} },
            Tab::About => rsx! { InfoIcon {} },
        }
    }

    fn body(self, local: Arc<PeerStats>) -> Element {
        match self {
            Tab::Profile => rsx! { profile::ProfileTab { local } },
            Tab::Keyboard => rsx! { keyboard::KeyboardTab {} },
            Tab::About => rsx! { about::AboutTab {} },
        }
    }
}

/// The window. `local` is our own entry, which the Profile tab names.
#[component]
pub fn SettingsModal(local: Arc<PeerStats>) -> Element {
    let panel = use_context::<SettingsPanel>();
    let focus = use_context::<KeyFocus>();
    let mut tab = use_signal(|| Tab::Profile);
    let open = (panel.open)();
    let current = tab();

    rsx! {
        div {
            class: if open {
                "fixed inset-0 z-30 flex items-center justify-center"
            } else {
                "hidden fixed inset-0 z-30 flex items-center justify-center"
            },
            // **The backdrop is a sibling under the panel, not its parent**:
            // a click on it closes, and a click in the panel never reaches it,
            // with no propagation to stop. The bar's menus do the same.
            div {
                class: "absolute inset-0 bg-black/60",
                onclick: move |_| panel.close(focus),
            }
            // Viewport units, not percentages: a percentage height has not
            // resolved reliably in blitz (see the app's root).
            div { class: "relative flex h-[80vh] max-h-[680px] w-[80vw] max-w-[920px] flex-row overflow-hidden rounded-xl border border-neutral-800 bg-neutral-900",
                div { class: "flex w-[200px] shrink-0 flex-col gap-1 border-r border-neutral-800 bg-neutral-950 p-3",
                    div { class: "px-2 pb-3 pt-1 text-base font-semibold text-neutral-100", "Settings" }
                    for item in Tab::ALL {
                        div {
                            key: "{item:?}",
                            // Plain colours, not palette shades, for the icon's
                            // sake: see `icons`.
                            class: if item == current {
                                "flex cursor-pointer flex-row items-center gap-2.5 rounded-md bg-neutral-800 px-2.5 py-2 text-sm text-white"
                            } else {
                                "flex cursor-pointer flex-row items-center gap-2.5 rounded-md px-2.5 py-2 text-sm text-[rgb(163,163,163)] hover:bg-neutral-900"
                            },
                            onclick: move |_| tab.set(item),
                            {item.icon()}
                            span { {item.label()} }
                        }
                    }
                }
                // The same thumb and track as every scrolling list in the app.
                div { class: "min-w-0 flex-1 overflow-y-auto overflow-x-hidden px-8 py-6 [scrollbar-color:rgb(115,115,115)_rgb(38,38,38)]",
                    {current.body(local.clone())}
                }
                // A way out that can be seen: Esc and the backdrop close it
                // too, but neither says so. Over the pane's corner, after it
                // so it is on top. A plain colour for the icon's sake.
                button {
                    class: "absolute right-3 top-3 flex h-8 w-8 cursor-pointer items-center justify-center rounded-md text-[rgb(163,163,163)] hover:bg-neutral-800",
                    onclick: move |_| panel.close(focus),
                    CloseIcon {}
                }
            }
        }
    }
}

/// A section's small capitalised heading. Capitalised in the text rather
/// than with `uppercase`, which is one less thing to depend on the renderer
/// for.
#[component]
fn Heading(text: &'static str) -> Element {
    let text = text.to_uppercase();
    rsx! {
        div { class: "mb-2 mt-6 text-xs font-medium tracking-wider text-neutral-500", "{text}" }
    }
}

/// One setting: its title, a line of description under it, and its control
/// at the right. `first` leaves off the divider above it; `notice` is a line
/// in red under the description, for something just refused.
#[component]
fn Row(
    title: String,
    description: String,
    first: bool,
    #[props(default)] notice: Option<String>,
    children: Element,
) -> Element {
    rsx! {
        div {
            class: if first {
                "flex flex-row items-center gap-4 py-3"
            } else {
                "flex flex-row items-center gap-4 border-t border-neutral-800 py-3"
            },
            div { class: "flex min-w-0 flex-1 flex-col gap-0.5",
                span { class: "text-sm text-neutral-100", "{title}" }
                span { class: "text-xs text-neutral-500", "{description}" }
                if let Some(notice) = notice {
                    span { class: "text-xs text-red-400", "{notice}" }
                }
            }
            div { class: "flex shrink-0 flex-row items-center gap-2", {children} }
        }
    }
}

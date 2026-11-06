//! The small ▾ beside a control-bar toggle, and the device list it opens.
//!
//! **Opens upward, floating.** It lives at the bottom of the window, so a menu
//! that dropped down would open off-screen. Floating needs blitz beta.4, where
//! absolutely positioned content receives clicks; on the old pin it painted
//! and ignored them.

use dioxus_native::prelude::*;

/// Which device menu is open, if any -- one value for the whole page.
///
/// **Shared, so switching menus is one click.** Each menu keeping its own open
/// flag meant the click on a second ▾ was spent closing the first. With one
/// value, opening a menu *is* closing the other. Provided by the page with
/// [`use_open_menu_provider`].
#[derive(Clone, Copy)]
pub struct OpenMenu(Signal<Option<&'static str>>);

/// Makes the shared [`OpenMenu`] available to every menu below.
pub fn use_open_menu_provider() {
    use_context_provider(|| OpenMenu(Signal::new(None)));
}

/// One choice in a [`DeviceMenu`]: a stable value plus the text shown for it.
///
/// The value is what identifies the choice; callers should use something that
/// survives the list being rebuilt (a device id, not a position).
#[derive(Clone, PartialEq)]
pub struct DropdownOption {
    /// Stable identity of this choice, reported back by `onchange`.
    pub value: String,
    /// Text shown for this choice.
    pub label: String,
}

/// One list in a [`DeviceMenu`], under its own heading -- the microphone's menu
/// has two, for the input and the output, as other calling apps do.
#[derive(Clone, PartialEq)]
pub struct MenuSection {
    /// Shown as the section's heading, e.g. "Microphone".
    pub title: &'static str,
    /// The devices to choose from.
    pub options: Vec<DropdownOption>,
    /// The `value` of the chosen one, if any.
    pub selected: Option<String>,
    /// Called with the chosen option's `value`.
    pub onchange: EventHandler<String>,
}

#[derive(Clone, PartialEq, Props)]
pub struct DeviceMenuProps {
    /// This menu's identity among the page's menus, e.g. "camera".
    pub name: &'static str,
    /// The lists, top to bottom.
    pub sections: Vec<MenuSection>,
    /// Looks for devices again; the menu's last item, covering every section.
    pub onrescan: EventHandler<()>,
}

#[component]
pub fn DeviceMenu(props: DeviceMenuProps) -> Element {
    let OpenMenu(mut current) = use_context::<OpenMenu>();
    let name = props.name;
    let open = move || current() == Some(name);

    rsx! {
        div { class: "relative",
            // `relative z-[6]` lifts every ▾ above the click-catching layer
            // below (z-5), so a click on the other menu's ▾ reaches it.
            button {
                class: if open() {
                    "relative z-[6] flex h-10 w-6 items-center justify-center rounded-md bg-neutral-700 text-xs text-neutral-200"
                } else {
                    "relative z-[6] flex h-10 w-6 items-center justify-center rounded-md text-xs text-neutral-400 hover:bg-neutral-800"
                },
                onclick: move |_| {
                    let next = if open() { None } else { Some(name) };
                    current.set(next);
                },
                "\u{25BE}"
            }
            // **Mounted always, hidden while closed -- never removed.** The
            // layer and the panel have positive z-index, so blitz caches them
            // in a stacking context; removing them left that cache naming a
            // node that was gone, and a hit-test before the next layout
            // panicked on it (blitz-dom `hit_inner`, finding F9). Hiding
            // changes a style, not the tree.
            Fragment {
                // **Closes on any click outside the menu.** An invisible layer
                // over the whole window, under the menu and the ▾ buttons: a
                // click anywhere else -- the video, the bar -- lands here and
                // only closes the menu. It does not go on to what is
                // underneath, which is the usual behaviour for a menu.
                div {
                    class: if open() { "fixed inset-0 z-[5]" } else { "hidden fixed inset-0 z-[5]" },
                    onclick: move |_| current.set(None),
                }
                div {
                    class: if open() {
                        "absolute bottom-full left-0 z-10 mb-2 w-72 rounded-lg border border-neutral-700 bg-neutral-900 p-1"
                    } else {
                        "hidden absolute bottom-full left-0 z-10 mb-2 w-72 rounded-lg border border-neutral-700 bg-neutral-900 p-1"
                    },
                    for (index, section) in props.sections.iter().enumerate() {
                        if index > 0 {
                            div { class: "my-1 h-px bg-neutral-800" }
                        }
                        div { class: "px-2.5 pb-1 pt-1.5 text-xs text-neutral-500", {section.title} }
                        if section.options.is_empty() {
                            div { class: "px-2.5 py-2 text-sm text-neutral-500", "None found" }
                        }
                        for option in section.options.iter() {
                            div {
                                class: if Some(&option.value) == section.selected.as_ref() {
                                    "rounded px-2.5 py-2 text-sm bg-blue-600 text-white"
                                } else {
                                    "rounded px-2.5 py-2 text-sm text-neutral-200 hover:bg-neutral-800"
                                },
                                onclick: {
                                    let value = option.value.clone();
                                    let onchange = section.onchange;
                                    move |_| {
                                        onchange.call(value.clone());
                                        current.set(None);
                                    }
                                },
                                {option.label.clone()}
                            }
                        }
                    }
                    div { class: "my-1 h-px bg-neutral-800" }
                    div {
                        class: "rounded px-2.5 py-2 text-sm text-neutral-400 hover:bg-neutral-800",
                        onclick: move |_| {
                            props.onrescan.call(());
                            current.set(None);
                        },
                        "Rescan devices"
                    }
                }
            }
        }
    }
}

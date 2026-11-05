use dioxus_native::prelude::*;

/// One choice in a [`Dropdown`]: a stable value plus the text shown for it.
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

/// A stand-in for `<select>`.
///
/// Blitz recognises the `select` tag for form submission but ships no
/// interactive widget for it, so the menu is assembled from plain elements.
#[derive(Clone, PartialEq, Props)]
pub struct DropdownProps {
    /// Text shown above the control.
    pub label: &'static str,
    /// Choices, in display order.
    pub options: Vec<DropdownOption>,
    /// Value of the current selection. Positions are deliberately not part of
    /// this API -- an index is only meaningful against one particular list, and
    /// the lists callers build get rebuilt underneath them.
    pub selected: Option<String>,
    /// Shown when `selected` names no option.
    pub placeholder: String,
    /// Fired with the newly picked value.
    pub onchange: EventHandler<String>,
}

#[component]
pub fn Dropdown(props: DropdownProps) -> Element {
    let mut open = use_signal(|| false);

    let selected_label = props
        .selected
        .as_ref()
        .and_then(|value| props.options.iter().find(|option| &option.value == value))
        .map(|option| option.label.clone())
        .unwrap_or_else(|| props.placeholder.clone());

    rsx! {
        div { class: "flex flex-col gap-1.5",
            span { class: "text-xs text-neutral-400", {props.label} }
            div { class: "relative w-64",
                div {
                    class: "flex flex-row items-center justify-between rounded-md border border-neutral-700 bg-neutral-900 px-3 py-2 text-sm hover:border-neutral-500",
                    onclick: move |_| open.set(!open()),
                    span { {selected_label} }
                    span { class: "text-[10px] text-neutral-400",
                        if open() { "\u{25B2}" } else { "\u{25BC}" }
                    }
                }
                if open() {
                    // Floats over the content below rather than pushing it down.
                    // Needs blitz v0.3.0-beta.4 or later: older revisions painted
                    // an absolutely positioned menu that overflows its parent but
                    // never delivered clicks to it, which is why this was once in
                    // normal flow.
                    div { class: "absolute left-0 top-full z-10 w-full mt-1 rounded-md border border-neutral-700 bg-neutral-900 p-1",
                        for option in props.options.iter() {
                            div {
                                class: if Some(&option.value) == props.selected.as_ref() {
                                    "rounded px-2.5 py-2 text-sm bg-blue-600"
                                } else {
                                    "rounded px-2.5 py-2 text-sm hover:bg-neutral-800"
                                },
                                onclick: {
                                    let value = option.value.clone();
                                    move |_| {
                                        props.onchange.call(value.clone());
                                        open.set(false);
                                    }
                                },
                                {option.label.clone()}
                            }
                        }
                    }
                }
            }
        }
    }
}

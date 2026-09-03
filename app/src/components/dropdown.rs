use dioxus::prelude::*;

/// A stand-in for `<select>`.
///
/// Blitz recognises the `select` tag for form submission but ships no
/// interactive widget for it, so the menu is assembled from plain elements.
#[derive(Clone, PartialEq, Props)]
pub struct DropdownProps {
    /// Text shown above the control.
    pub label: &'static str,
    /// Choices, in display order.
    pub options: Vec<String>,
    /// Index into `options`.
    pub selected: usize,
    /// Shown when `selected` does not name an option.
    pub placeholder: String,
    /// Fired with the newly picked index.
    pub onchange: EventHandler<usize>,
}

#[component]
pub fn Dropdown(props: DropdownProps) -> Element {
    let mut open = use_signal(|| false);

    let selected_label = props
        .options
        .get(props.selected)
        .cloned()
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
                    // Catches clicks anywhere else on the page so the menu closes.
                    div { class: "fixed inset-0 z-10", onclick: move |_| open.set(false) }
                    div { class: "absolute inset-x-0 top-11 z-20 rounded-md border border-neutral-700 bg-neutral-900 p-1",
                        for (index, option) in props.options.iter().enumerate() {
                            div {
                                class: if index == props.selected {
                                    "rounded px-2.5 py-2 text-sm bg-blue-600"
                                } else {
                                    "rounded px-2.5 py-2 text-sm hover:bg-neutral-800"
                                },
                                onclick: move |_| {
                                    props.onchange.call(index);
                                    open.set(false);
                                },
                                {option.clone()}
                            }
                        }
                    }
                }
            }
        }
    }
}

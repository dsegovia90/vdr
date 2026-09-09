//! The one button.
//!
//! Exists as much for the class list as for the markup. Every button in this app
//! carries `shrink-0 whitespace-nowrap`, and both are load-bearing rather than
//! decorative: without them a button in a flex row is sized below its own text
//! and the label spills outside the background. That was a live bug, and it was
//! a bug **per button** — fixing one did nothing for the next. Here there is one
//! place to get it right.
//!
//! `whitespace-nowrap` over `text-nowrap`: Tailwind v4 emits `white-space` for
//! the former and `text-wrap` for the latter, and `white-space` expands in stylo
//! to *both* `text-wrap-mode` and `white-space-collapse`, which is the pair
//! blitz actually reads.

use dioxus_native::prelude::*;

// Every class below is a *whole literal*. Tailwind's compiler finds classes by
// scanning source text, so it can see `"bg-blue-600"` here even though the
// string is only assembled at runtime. Build one by pieces --
// `format!("bg-{tone}-600")` -- and the class is never generated, the rule never
// reaches the stylesheet, and the button renders unstyled with nothing to
// suggest why.

/// How much the button is asking for.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Tone {
    /// The action the user most likely came for. One per row, at most.
    Primary,
    /// Everything else.
    #[default]
    Secondary,
}

/// How much room it takes.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Size {
    /// The default, for anything in a control row.
    #[default]
    Normal,
    /// For a button that sits beside text rather than beneath it, like `Copy`
    /// next to the key it copies.
    Small,
}

impl Tone {
    fn classes(self) -> &'static str {
        match self {
            Self::Primary => "bg-blue-600 text-white hover:bg-blue-500",
            Self::Secondary => "bg-neutral-800 text-neutral-100 hover:bg-neutral-700",
        }
    }
}

impl Size {
    fn classes(self) -> &'static str {
        match self {
            Self::Normal => "px-4 py-2 text-sm",
            Self::Small => "px-3 py-1 text-xs",
        }
    }
}

/// A button.
///
/// Takes children rather than a label string so a caller can vary the text
/// inline — `if on { "Turn camera off" } else { "Turn camera on" }` — which is
/// what the camera toggle already did before this existed.
#[component]
pub fn Button(
    /// What pressing it does.
    onclick: EventHandler<MouseEvent>,
    /// Defaults to [`Tone::Secondary`].
    #[props(default)]
    tone: Tone,
    /// Defaults to [`Size::Normal`].
    #[props(default)]
    size: Size,
    /// The label.
    children: Element,
) -> Element {
    rsx! {
        button {
            // `shrink-0` and `whitespace-nowrap` are not styling -- see the
            // module docs. Do not move them to a caller.
            class: "shrink-0 whitespace-nowrap rounded-md {size.classes()} {tone.classes()}",
            onclick: move |event| onclick.call(event),
            {children}
        }
    }
}

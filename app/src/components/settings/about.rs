//! The About tab: which build this is, where its files are, and its licence.
//!
//! The log's path is the one thing here someone is likely to need: it is what
//! to send after a call went wrong, and a double-clicked app has no terminal
//! that would have shown it.

use dioxus_native::prelude::*;

use super::super::Button;
use super::super::Size;
use super::super::connect::copy_to_clipboard;
use super::{Heading, Row};
use crate::settings;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const REPOSITORY: &str = "https://github.com/dsegovia90/vdr";

#[component]
pub fn AboutTab() -> Element {
    // Displayed, never created: `log_file` names the path without touching
    // the disk.
    let folder = settings::data_dir().map(|path| path.display().to_string());
    let log = settings::log_file().map(|path| path.display().to_string());

    rsx! {
        div { class: "flex flex-col",
            div { class: "text-lg font-semibold text-neutral-100", "vdr" }
            p { class: "mt-1 text-sm text-neutral-400", "Version {VERSION}" }

            Heading { text: "Files" }
            Row {
                title: "Data folder",
                description: folder.clone().unwrap_or_else(|| "None on this system".to_string()),
                first: true,
                CopyButton { text: folder }
            }
            Row {
                title: "Log file",
                description: log.clone().unwrap_or_else(|| "Logging to the terminal only".to_string()),
                first: false,
                CopyButton { text: log }
            }
            p { class: "text-xs text-neutral-500",
                "After a call that went wrong, the log is what to send. The last three runs are kept beside it."
            }

            Heading { text: "Source" }
            Row {
                title: "Repository",
                description: REPOSITORY,
                first: true,
                CopyButton { text: Some(REPOSITORY.to_string()) }
            }

            Heading { text: "Licence" }
            p { class: "text-sm text-neutral-300",
                "vdr is free software under the MIT licence or the Apache License 2.0, at your choice (LICENSE-MIT, LICENSE-APACHE)."
            }
            p { class: "mt-2 text-xs text-neutral-500",
                "It includes third-party software under its own licences, listed in THIRD_PARTY_LICENSES, in the repository and in each download beside the app."
            }
        }
    }
}

/// Copies `text`, or shows nothing when there is nothing to copy.
#[component]
fn CopyButton(text: Option<String>) -> Element {
    let Some(text) = text else { return rsx! {} };
    rsx! {
        Button {
            size: Size::Small,
            onclick: move |_| copy_to_clipboard(&text),
            "Copy"
        }
    }
}

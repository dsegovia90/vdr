//! The Updates tab: which version this is, whether there is a newer one,
//! and taking it.
//!
//! The work is `crate::update`'s, on threads of its own; this draws the
//! [`Status`] it sends. Installing restarts the app, so the button is
//! dimmed during a call, and the updater checks again before it changes
//! anything.
//!
//! **The release notes are a link** the system's browser opens (see
//! `update::link` for which URLs it will open). One it refuses, or an opener
//! that will not start, falls back to the URL as text with a Copy button.
//!
//! The banner on the idle screen lives here too, since it is this state.

use dioxus_native::prelude::*;

use super::super::connect::copy_to_clipboard;
use super::super::{Button, SettingsPanel, Size, Tone};
use super::{Heading, Row, Tab};
use crate::channel::use_channel;
use crate::net::RemoteStatus;
use crate::settings::Settings;
use crate::update::{self, Ability, Phase, Status, Updater, capitalised};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The updater and what it last said, shared by this tab and the dot on
/// the settings button.
#[derive(Clone)]
pub struct Updates {
    pub status: Signal<Status>,
    /// The call's status: no installing while it is under way.
    call: Signal<RemoteStatus>,
    auto: Signal<bool>,
    /// The newest version whose banner was dismissed, as saved.
    dismissed: Signal<Option<String>>,
    updater: Updater,
}

/// Starts the updater's schedule and makes [`Updates`] available below.
pub fn use_updates_provider(call: Signal<RemoteStatus>) -> Updates {
    let mut status = use_signal(Status::default);
    let tx = use_channel(move |next: Status| status.set(next));
    let saved = use_hook(Settings::load);
    let auto = use_signal(|| saved.checks_updates());
    let dismissed = use_signal(|| saved.dismissed_update.clone());
    use_context_provider(move || Updates {
        status,
        call,
        auto,
        dismissed,
        updater: Updater::start(tx.clone(), *auto.peek()),
    })
}

#[component]
pub fn UpdatesTab() -> Element {
    let Updates {
        status,
        call,
        mut auto,
        updater,
        ..
    } = use_context::<Updates>();
    let now = status();
    let in_call = call.read().is_cancellable();
    let auto_on = auto();
    let working = matches!(
        now.phase,
        Phase::Checking | Phase::Downloading(_) | Phase::Verifying | Phase::Installing | Phase::Restarting
    );
    let (state, failed) = describe(&now.phase);

    rsx! {
        div { class: "flex flex-col",
            div { class: "text-lg font-semibold text-neutral-100", "Updates" }
            p { class: "mt-1 text-sm text-neutral-400", "This is vdr {VERSION}." }

            Heading { text: "Status" }
            Row {
                title: state,
                description: "Checks ask GitHub for the latest release. Nothing is downloaded until you choose to update.",
                first: true,
                notice: failed,
                Button {
                    size: Size::Small,
                    disabled: working,
                    onclick: {
                        let updater = updater.clone();
                        move |_| updater.check_now()
                    },
                    "Check now"
                }
            }
            if let Some(offer) = now.offer.clone() {
                Offered {
                    version: offer.version,
                    page: offer.page,
                    phase: now.phase.clone(),
                    ability: now.ability.clone(),
                    in_call,
                    working,
                    updater: updater.clone(),
                }
            }

            Heading { text: "Automatic check" }
            Row {
                title: "Check for updates automatically",
                description: "Shortly after vdr starts, then once a day. Only a check: updating always waits for you.",
                first: true,
                Switch {
                    on: auto_on,
                    onclick: {
                        let updater = updater.clone();
                        move |_| {
                            let next = !*auto.peek();
                            auto.set(next);
                            updater.set_auto(next);
                            Settings::update(move |settings| settings.check_updates = Some(next));
                        }
                    },
                }
            }
            p { class: "mt-4 text-xs text-neutral-500",
                "Updates are signed. vdr checks the signature and the download's checksum before it unpacks anything, and refuses a package that does not match."
            }
        }
    }
}

/// The phase in words, and a failure's reason for the red line under it.
fn describe(phase: &Phase) -> (String, Option<String>) {
    match phase {
        Phase::Idle => ("Not checked yet".into(), None),
        Phase::Checking => ("Checking\u{2026}".into(), None),
        Phase::UpToDate => ("Up to date".into(), None),
        Phase::CouldNotCheck(reason) => ("Could not check".into(), Some(capitalised(reason))),
        Phase::Available => ("A newer version is available".into(), None),
        Phase::Ready => ("Downloaded and verified, ready to install".into(), None),
        Phase::Downloading(percent) => (format!("Downloading\u{2026} {percent}%"), None),
        Phase::Verifying => ("Checking the signature\u{2026}".into(), None),
        Phase::Installing => ("Installing\u{2026}".into(), None),
        Phase::Restarting => ("Restarting\u{2026}".into(), None),
        Phase::Downloaded(_) => ("Downloaded and verified".into(), None),
        Phase::Installed => ("Installed: quit vdr and open it again to finish".into(), None),
        Phase::Failed(reason) => ("The update failed".into(), Some(capitalised(reason))),
    }
}

/// The newer release: what can be done with it here, and its notes.
#[component]
fn Offered(
    version: String,
    page: String,
    phase: Phase,
    ability: Ability,
    in_call: bool,
    working: bool,
    updater: Updater,
) -> Element {
    let downloaded = match &phase {
        Phase::Downloaded(path) => Some(path.display().to_string()),
        _ => None,
    };
    let (description, button) = match &ability {
        Ability::Unknown => ("Looking at how this copy was installed\u{2026}".to_string(), None),
        Ability::Install if in_call => (
            "Not during a call: updating restarts vdr. It will be here when the call ends.".to_string(),
            Some(("Update and restart", true)),
        ),
        Ability::Install => (
            "Downloads it, checks its signature, replaces this copy and restarts vdr.".to_string(),
            Some(("Update and restart", working || phase == Phase::Installed)),
        ),
        Ability::DownloadOnly(reason) => (
            format!("{reason} vdr can download and check it, and you install it by hand."),
            Some(("Download", working || downloaded.is_some())),
        ),
        Ability::Refused(reason) => (format!("{reason} Get it from the release page."), None),
    };
    rsx! {
        Row {
            title: "vdr {version}",
            description,
            first: false,
            if let Some((label, disabled)) = button {
                Button {
                    tone: Tone::Primary,
                    disabled,
                    onclick: move |_| updater.install(),
                    "{label}"
                }
            }
        }
        if let Some(path) = downloaded {
            Row {
                title: "Downloaded to",
                description: path.clone(),
                first: false,
                Button {
                    size: Size::Small,
                    onclick: move |_| copy_to_clipboard(&path),
                    "Copy"
                }
            }
        }
        ReleaseNotes { page }
    }
}

/// An on/off switch. A plain element rather than a [`Button`]: it has no
/// label to keep from wrapping, and its look is its state.
#[component]
fn Switch(on: bool, onclick: EventHandler<MouseEvent>) -> Element {
    rsx! {
        div {
            class: if on {
                "relative h-6 w-11 shrink-0 cursor-pointer rounded-full bg-blue-600"
            } else {
                "relative h-6 w-11 shrink-0 cursor-pointer rounded-full bg-neutral-700"
            },
            onclick: move |event| onclick.call(event),
            div {
                class: if on {
                    "absolute left-[22px] top-[2px] h-5 w-5 rounded-full bg-white"
                } else {
                    "absolute left-[2px] top-[2px] h-5 w-5 rounded-full bg-white"
                },
            }
        }
    }
}

/// The release page as a link, or, once opening it has failed, as text to
/// copy. Failing is remembered for as long as the tab is mounted: a link that
/// did nothing once will do nothing again.
#[component]
fn ReleaseNotes(page: String) -> Element {
    let mut unopenable = use_signal(|| false);
    if unopenable() {
        return rsx! {
            Row {
                title: "Release notes",
                description: page.clone(),
                first: false,
                Button {
                    size: Size::Small,
                    onclick: move |_| copy_to_clipboard(&page),
                    "Copy"
                }
            }
        };
    }
    rsx! {
        div { class: "flex flex-row items-center gap-4 border-t border-neutral-800 py-3",
            // A plain colour, and the underline, are what say "link" here:
            // there is no tooltip to.
            span {
                class: "cursor-pointer text-sm text-[rgb(96,165,250)] underline hover:text-[rgb(147,197,253)]",
                onclick: move |_| {
                    if let Err(error) = update::open_release_page(&page) {
                        tracing::warn!("update: could not open the release notes: {error}");
                        unopenable.set(true);
                    }
                },
                "Release notes"
            }
        }
    }
}

/// "Version X is available" above the connect controls: outside a call
/// only, until dismissed for that version. Update opens the Updates tab; it
/// never installs from here.
#[component]
pub fn UpdateBanner() -> Element {
    let Updates {
        status,
        call,
        mut dismissed,
        ..
    } = use_context::<Updates>();
    let panel = use_context::<SettingsPanel>();
    // A memo over state that is already there: no messages of its own, and a
    // change of percent mid-download does not redraw it.
    let shown = use_memo(move || update::banner(&status.read(), dismissed.read().as_deref(), call.read().is_cancellable()));
    let Some(version) = shown() else { return rsx! {} };
    rsx! {
        div { class: "flex shrink-0 flex-row items-center gap-3 rounded-md border border-neutral-800 bg-neutral-900 px-3 py-2",
            span { class: "h-2 w-2 shrink-0 rounded-full bg-blue-500" }
            span { class: "min-w-0 flex-1 truncate text-sm text-neutral-200", "Version {version} is available" }
            Button {
                size: Size::Small,
                tone: Tone::Primary,
                onclick: move |_| panel.show_tab(Tab::Updates),
                "Update"
            }
            Button {
                size: Size::Small,
                onclick: move |_| {
                    let version = version.clone();
                    dismissed.set(Some(version.clone()));
                    Settings::update(move |settings| settings.dismissed_update = Some(version));
                },
                "Dismiss"
            }
        }
    }
}

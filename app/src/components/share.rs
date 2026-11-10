//! Sharing a screen, as the page shows it: the bar's Share button with its ▾,
//! and a shared screen's tile.

use std::time::Duration;

use dioxus_native::prelude::*;

use super::device_menu::OpenMenu;
use super::{DeviceMenu, DropdownOption, MenuSection, OwnShare, ParticipantVideo, ScreenIcon};
use crate::camera::{Fit, FrameSlot, Mirror};
use crate::channel::use_channel;
use crate::net::{ShareOption, ShareSource, ShareWish, share_lists, share_sources};

/// This menu's name among the page's menus.
const NAME: &str = "share";

/// How often the button re-reads what we asked for. A share that ends by
/// itself sets the wish back to nothing off the interface's thread.
const POLL: Duration = Duration::from_millis(250);

#[derive(Clone, PartialEq, Props)]
pub struct ShareControlProps {
    /// What we want to share; the call serves it.
    pub share: ShareWish,
    /// Whether we are on a call. Off a call the control is hidden, not
    /// removed: its menu carries a positive z-index (see `device_menu`).
    pub shown: bool,
}

/// The Share button, and where the platform lists them, a ▾ of displays and
/// windows.
///
/// The button shares the main display, or on Wayland opens the system's own
/// picker; again, it stops. Picking from the ▾ shares that instead, replacing
/// any share already running.
#[component]
pub fn ShareControl(props: ShareControlProps) -> Element {
    let OwnShare(own) = use_context::<OwnShare>();
    let OpenMenu(mut current) = use_context::<OpenMenu>();
    let mut wish = use_signal(|| None::<ShareSource>);
    let mut displays = use_signal(Vec::<ShareOption>::new);
    let mut windows = use_signal(Vec::<ShareOption>::new);
    let mut listed = use_signal(|| false);
    let shown = props.shown;

    // **Closed when the call goes**, as the subroom menu is.
    use_effect(use_reactive!(|shown| {
        if !shown && current.peek().as_deref() == Some(NAME) {
            current.set(None);
        }
    }));

    let wishes = use_channel(move |next: Option<ShareSource>| {
        if *wish.peek() != next {
            wish.set(next);
        }
    });
    use_hook({
        let share = props.share.clone();
        move || {
            let spawned = std::thread::Builder::new()
                .name("share-wish".into())
                .spawn(move || loop {
                    if wishes.unbounded_send(share.get()).is_err() {
                        return;
                    }
                    std::thread::sleep(POLL);
                });
            if let Err(error) = spawned {
                tracing::warn!("could not spawn the share button's thread: {error}");
            }
        }
    });

    let found = use_channel(move |(screens, apps): (Vec<ShareOption>, Vec<ShareOption>)| {
        displays.set(screens);
        windows.set(apps);
    });
    let list = use_callback(move |()| {
        let found = found.clone();
        let spawned = std::thread::Builder::new()
            .name("share-list".into())
            .spawn(move || {
                let _ = found.unbounded_send(share_sources());
            });
        if let Err(error) = spawned {
            tracing::warn!("could not spawn the share listing thread: {error}");
        }
    });
    // **Listed on the first open of the ▾, not when the call starts.** On macOS
    // listing needs the screen-recording grant, so it can put up the system's
    // alert, and joining a call must not do that to someone who never meant to
    // share.
    use_effect(move || {
        if current.read().as_deref() == Some(NAME) && !*listed.peek() {
            listed.set(true);
            list.call(());
        }
    });

    let (sharing, _) = own();
    let wanted = wish();
    // On from the click, before the capture has opened, so the button answers
    // at once; and while the share runs.
    let on = sharing || wanted.is_some();
    let selected = wanted.as_ref().map(ShareSource::key);
    let options = |list: &[ShareOption]| -> Vec<DropdownOption> {
        list.iter()
            .map(|option| DropdownOption {
                value: option.source.key(),
                label: option.label.clone(),
            })
            .collect()
    };
    let pick = {
        let share = props.share.clone();
        move |value: String| {
            let chosen = displays
                .peek()
                .iter()
                .chain(windows.peek().iter())
                .find(|option| option.source.key() == value)
                .map(|option| option.source.clone());
            if let Some(source) = chosen {
                share.set(Some(source.clone()));
                wish.set(Some(source));
            }
        }
    };

    rsx! {
        div { class: if shown { "flex shrink-0 flex-row items-center gap-0.5" } else { "hidden flex shrink-0 flex-row items-center gap-0.5" },
            button {
                class: if on {
                    "flex h-10 w-10 cursor-pointer items-center justify-center rounded-full bg-emerald-600 text-white hover:bg-emerald-500"
                } else {
                    "flex h-10 w-10 cursor-pointer items-center justify-center rounded-full bg-neutral-800 text-white hover:bg-neutral-700"
                },
                onclick: {
                    let share = props.share.clone();
                    move |_| {
                        let next = if on { None } else { Some(ShareSource::Display(None)) };
                        share.set(next.clone());
                        wish.set(next);
                    }
                },
                ScreenIcon { on }
            }
            // Where the system's own picker chooses (Wayland), there is
            // nothing to list, and the button alone opens the picker. A
            // platform fact, fixed for the run, so this never adds or removes
            // a node while the page is up.
            if share_lists() {
                DeviceMenu {
                    name: NAME,
                    sections: vec![
                        MenuSection {
                            title: "Screens",
                            options: options(&displays.read()),
                            selected: selected.clone(),
                            onchange: EventHandler::new(pick.clone()),
                        },
                        MenuSection {
                            title: "Windows",
                            options: options(&windows.read()),
                            selected,
                            onchange: EventHandler::new(pick),
                        },
                    ],
                    onrescan: move |_| list.call(()),
                }
            }
        }
    }
}

#[derive(Clone, PartialEq, Props)]
pub struct ShareTileProps {
    /// The label's text, e.g. "3929461d · screen".
    pub label: String,
    /// Their decoded screen, or `None` for our own share, which we do not
    /// show back to ourselves: a self-view of a screen is a tunnel of mirrors
    /// and a second decode.
    pub slot: Option<FrameSlot>,
    /// Size and placement, `layout::TILE`.
    pub class: &'static str,
    /// The dim layer, as their camera's tile has it.
    pub dim: &'static str,
}

/// A shared screen, as one more tile beside its sharer's camera.
///
/// **Not a `ParticipantTile`**: that writes its person's room into the page's
/// map and removes it when it unmounts, so a share ending would delete its
/// sharer's room.
#[component]
pub fn ShareTile(props: ShareTileProps) -> Element {
    rsx! {
        div { class: props.class,
            if let Some(slot) = props.slot.clone() {
                ParticipantVideo {
                    label: props.label.clone(),
                    slot,
                    // Never mirrored and never cropped: it is text and
                    // windows, the right way round, to the edge.
                    mirror: Mirror::None,
                    fit: Fit::Contain,
                    class: "absolute inset-0",
                }
            } else {
                div { class: "absolute inset-0 flex items-center justify-center rounded-[12px] bg-neutral-800 text-sm text-neutral-300",
                    "Sharing your screen"
                }
            }
            div { class: "absolute bottom-2 left-2 rounded-md bg-black/50 px-2 py-1",
                span { class: "whitespace-nowrap text-xs text-neutral-200", "{props.label}" }
            }
            // Last, so it covers the rest; see `ParticipantTile`.
            div { class: props.dim }
        }
    }
}

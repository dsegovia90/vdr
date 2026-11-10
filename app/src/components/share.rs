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
/// **The button never starts a share by itself.** Not sharing, it opens the
/// list -- what "the main display" means is not obvious on a Mac with several,
/// and guessing confused the first person to try it -- and picking from the
/// list shares. Sharing, it stops. The ▾ opens the same list, to switch source.
/// Where there is no list (Wayland, where the system's portal is the picker),
/// the button starts the portal directly.
#[component]
pub fn ShareControl(props: ShareControlProps) -> Element {
    let OwnShare(own) = use_context::<OwnShare>();
    let OpenMenu(mut current) = use_context::<OpenMenu>();
    let mut wish = use_signal(|| None::<ShareSource>);
    // The wish's generation as of our own last click. A poll that read the
    // wish before the click must not undo what the click showed.
    let mut clicked = use_signal(|| 0u64);
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

    let wishes = use_channel(move |(generation, next): (u64, Option<ShareSource>)| {
        if generation >= *clicked.peek() && *wish.peek() != next {
            wish.set(next);
        }
    });
    use_hook({
        let share = props.share.clone();
        move || {
            let spawned = std::thread::Builder::new()
                .name("share-wish".into())
                .spawn(move || loop {
                    // The generation first: the value read after it is at
                    // least that new (see `ShareWish`).
                    let generation = share.generation();
                    if wishes.unbounded_send((generation, share.get())).is_err() {
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
                clicked.set(share.generation());
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
                        if !on && share_lists() {
                            // The ▾'s own list, opened from here: the menu is
                            // the ▾'s, keyed by name, so this is all it takes.
                            let next = if current.peek().as_deref() == Some(NAME) { None } else { Some(NAME.to_string()) };
                            current.set(next);
                            return;
                        }
                        let next = if on { None } else { Some(ShareSource::Display(None)) };
                        share.set(next.clone());
                        clicked.set(share.generation());
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
    /// The screen's frames: a peer's decoded, or our own capture's preview.
    pub slot: FrameSlot,
    /// Size and placement: `layout::TILE`, `SHARE` or `STRIP`.
    pub class: &'static str,
    /// The dim layer, as their camera's tile has it.
    pub dim: &'static str,
    /// Our own share's Stop, which makes the card ours: it says so and
    /// offers the button. `None` on a peer's.
    #[props(default)]
    pub stop: Option<EventHandler<()>>,
}

/// A shared screen's card: a peer's, or our own with its Stop button.
///
/// **Not a `ParticipantTile`**: that writes its person's room into the page's
/// map and removes it when it unmounts, so a share ending would delete its
/// sharer's room.
#[component]
pub fn ShareTile(props: ShareTileProps) -> Element {
    rsx! {
        div { class: props.class,
            ParticipantVideo {
                label: props.label.clone(),
                slot: props.slot.clone(),
                // Never mirrored and never cropped: it is text and windows,
                // the right way round, to the edge.
                mirror: Mirror::None,
                fit: Fit::Contain,
                class: "absolute inset-0",
            }
            if let Some(stop) = props.stop {
                div { class: "absolute bottom-2 left-2 flex flex-row items-center gap-2 rounded-md bg-black/50 py-1 pl-2 pr-1",
                    span { class: "whitespace-nowrap text-xs text-neutral-200", "You are sharing your screen" }
                    button {
                        class: "rounded bg-red-600 px-2 py-0.5 text-xs text-white hover:bg-red-500",
                        onclick: move |_| stop.call(()),
                        "Stop"
                    }
                }
            } else {
                div { class: "absolute bottom-2 left-2 rounded-md bg-black/50 px-2 py-1",
                    span { class: "whitespace-nowrap text-xs text-neutral-200", "{props.label}" }
                }
            }
            // Last, so it covers the rest; see `ParticipantTile`. Our own card
            // is always in our room, so never dimmed, and its Stop is never
            // under a layer.
            div { class: props.dim }
        }
    }
}

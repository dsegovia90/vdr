//! The plumbing both hooks need to get work off the UI thread and back.

use dioxus::prelude::*;
use futures_channel::mpsc::{unbounded, UnboundedSender};
use futures_util::StreamExt;

/// Opens a channel drained on the UI thread and returns its sender.
///
/// Signals are `!Send` and the platform calls that produce these values block,
/// so the work has to happen on a worker thread and be carried back here. Hand
/// the returned sender to those threads.
///
/// Covers the channel setup only, deliberately. `use_devices` spawns a thread
/// per refresh while `use_camera_stream` spawns one per source change and tags
/// it with a generation counter; a helper that also owned the threading policy
/// would fit neither.
pub(super) fn use_channel<T, F>(mut on_message: F) -> UnboundedSender<T>
where
    T: 'static,
    F: FnMut(T) + 'static,
{
    use_hook(|| {
        let (tx, mut rx) = unbounded::<T>();
        spawn(async move {
            while let Some(message) = rx.next().await {
                on_message(message);
            }
        });
        tx
    })
}

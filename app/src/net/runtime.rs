//! The one tokio runtime this app has.
//!
//! Nothing in the preview path is async: the camera pump is an OS thread and
//! the renderer is a callback. But `moq-media`'s decode pipeline calls
//! `tokio::spawn` while it is being built, so constructing one needs a runtime
//! in context -- there is no non-async way in. That is true of the in-process
//! loopback as much as of anything over the network.

use std::sync::OnceLock;

use tokio::runtime::Runtime;

/// Returns the shared runtime, or `None` if it could not be built.
///
/// **Deliberately never dropped.** `Runtime::drop` blocks until every spawned
/// task has parked, so a runtime owned by a component would block the UI thread
/// on unmount -- exactly the thing the whole camera teardown design avoids. A
/// `OnceLock` never runs a destructor, and the process exiting is what reclaims
/// it. The cost is that runtime shutdown never happens; the benefit is that it
/// can never happen on the wrong thread.
///
/// Multi-threaded rather than current-thread: the encode and decode pipelines
/// each own an OS thread of their own and use tokio only for packet forwarding,
/// but nothing here is ever driven by a caller blocking on it, so a runtime
/// with no worker threads would simply never poll them.
pub fn runtime() -> Option<&'static Runtime> {
    static RUNTIME: OnceLock<Option<Runtime>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| match Runtime::new() {
            Ok(runtime) => Some(runtime),
            Err(error) => {
                tracing::error!("could not start the tokio runtime: {error}");
                None
            }
        })
        .as_ref()
}

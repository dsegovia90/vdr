//! Keeping the machine and its screen awake while a call is up.
//!
//! The app is meant to sit in a call all day, and an OS that idles out
//! freezes the tile or drops the person from the room. The screen is kept on
//! as well as the system: a dark screen in front of a live camera is a poor
//! signal to everyone else.
//!
//! Held only while connected (and through a reconnect), never while idle or
//! connecting, so an open window does not pin the machine. [`follow`] is what
//! decides that, from the statuses the call publishes.

use futures_channel::mpsc::{UnboundedReceiver, UnboundedSender};
use futures_util::StreamExt as _;

use crate::net::RemoteStatus;

/// What the OS is told, where it shows a reason (`pmset -g assertions`,
/// `systemd-inhibit --list`, a desktop's inhibitor list).
const REASON: &str = "vdr: in a call";

/// Keeps the system and the display from idling out until dropped.
///
/// The field is never read: holding it is the point, and its drop is the release.
pub struct Guard(#[allow(dead_code)] platform::Held);

impl Guard {
    /// Asks the OS to stay awake, screen included.
    ///
    /// `None` when it refused or there is nothing to ask; the reason is logged
    /// here at `warn`, and the call goes on without it.
    ///
    /// Async because the Linux path is D-Bus; on macOS it never waits.
    pub async fn take() -> Option<Guard> {
        match platform::take().await {
            Ok(held) => {
                tracing::debug!("[awake] keeping the machine awake: {}", platform::NAME);
                Some(Guard(held))
            }
            Err(error) => {
                tracing::warn!("[awake] could not keep the machine awake; it may sleep mid-call: {error}");
                None
            }
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        tracing::debug!("[awake] released; the machine may sleep again");
    }
}

/// Forwards every call status from `statuses` to `out`, holding a [`Guard`]
/// while they say a call is up.
///
/// Taken on the first `Connected` / `InRoom` of a call, kept through
/// `Reconnecting` (the call is not over), dropped on anything else: back to
/// idle, ended, failed, or a new attempt starting. Tried once per call, so a
/// refusal is one warning rather than one per reconnect.
///
/// Forwarded before acting, so a slow D-Bus round trip never holds back what
/// the interface shows.
pub async fn follow(mut statuses: UnboundedReceiver<RemoteStatus>, out: UnboundedSender<RemoteStatus>) {
    let mut guard: Option<Guard> = None;
    let mut tried = false;
    while let Some(next) = statuses.next().await {
        let connected = next.is_connected();
        let in_call = connected || matches!(next, RemoteStatus::Reconnecting);
        let _ = out.unbounded_send(next);
        if connected && !tried {
            tried = true;
            guard = Guard::take().await;
        } else if !in_call {
            tried = false;
            guard = None;
        }
    }
    drop(guard);
}

/// One IOKit power assertion.
///
/// `PreventUserIdleDisplaySleep` keeps the display on, and a display that is
/// on keeps the system up with it, so one assertion covers both. It does not
/// stop a MacBook sleeping when its lid closes: nothing an app asks for does.
#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::{CString, c_char, c_void};

    pub const NAME: &str = "IOKit assertion PreventUserIdleDisplaySleep";

    type CFStringRef = *const c_void;
    type IOPMAssertionID = u32;
    type IOReturn = i32;

    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    const K_IOPM_ASSERTION_LEVEL_ON: u32 = 255;
    const K_IO_RETURN_SUCCESS: IOReturn = 0;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(
            alloc: *const c_void,
            string: *const c_char,
            encoding: u32,
        ) -> CFStringRef;
        fn CFRelease(cf: *const c_void);
    }

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionCreateWithName(
            kind: CFStringRef,
            level: u32,
            name: CFStringRef,
            id: *mut IOPMAssertionID,
        ) -> IOReturn;
        fn IOPMAssertionRelease(id: IOPMAssertionID) -> IOReturn;
    }

    /// An owned `CFString`, released on drop.
    struct CfString(CFStringRef);

    impl CfString {
        fn new(text: &str) -> Result<Self, String> {
            let text = CString::new(text).map_err(|error| error.to_string())?;
            // SAFETY: a valid NUL-terminated string; a null allocator means
            // the default one. We own the result (the Create rule).
            let string = unsafe {
                CFStringCreateWithCString(std::ptr::null(), text.as_ptr(), K_CF_STRING_ENCODING_UTF8)
            };
            if string.is_null() {
                Err("CFStringCreateWithCString returned null".to_string())
            } else {
                Ok(Self(string))
            }
        }
    }

    impl Drop for CfString {
        fn drop(&mut self) {
            // SAFETY: created non-null by us and released exactly once.
            unsafe { CFRelease(self.0) }
        }
    }

    pub struct Held(IOPMAssertionID);

    pub async fn take() -> Result<Held, String> {
        let kind = CfString::new("PreventUserIdleDisplaySleep")?;
        let name = CfString::new(super::REASON)?;
        let mut id: IOPMAssertionID = 0;
        // SAFETY: both strings live past the call, which copies what it keeps;
        // `id` is a valid out pointer.
        let result =
            unsafe { IOPMAssertionCreateWithName(kind.0, K_IOPM_ASSERTION_LEVEL_ON, name.0, &mut id) };
        if result == K_IO_RETURN_SUCCESS {
            Ok(Held(id))
        } else {
            Err(format!("IOPMAssertionCreateWithName returned {result:#x}"))
        }
    }

    impl Drop for Held {
        fn drop(&mut self) {
            // SAFETY: an id IOKit gave us, released once.
            let result = unsafe { IOPMAssertionRelease(self.0) };
            if result != K_IO_RETURN_SUCCESS {
                tracing::warn!("[awake] IOPMAssertionRelease returned {result:#x}");
            }
        }
    }
}

/// The desktop portal's `Inhibit` with the idle flag, or logind's where there
/// is no portal.
///
/// The portal is the one desktop-neutral way to stop the screen blanking: the
/// desktop's own portal backend passes it to whatever runs its idle timer.
/// logind's inhibitor (`idle:sleep`) keeps the system up but cannot reach the
/// compositor's screen blanking, which has no neutral interface outside the
/// portal; without a portal, the screen may still go dark.
#[cfg(target_os = "linux")]
mod platform {
    use ashpd::desktop::Request;
    use ashpd::desktop::inhibit::{InhibitFlags, InhibitOptions, InhibitProxy};

    pub const NAME: &str = "inhibitor (portal, else logind)";

    pub enum Held {
        /// Lasts until the request is closed. The portal connection is
        /// ashpd's shared one and outlives us, so it must be closed by hand.
        Portal(Option<Box<Request<()>>>),
        /// Lasts until this descriptor is closed, which dropping it does.
        Logind(#[allow(dead_code)] zbus::zvariant::OwnedFd),
    }

    pub async fn take() -> Result<Held, String> {
        let portal = match portal().await {
            Ok(request) => return Ok(Held::Portal(Some(Box::new(request)))),
            Err(error) => error,
        };
        tracing::debug!("[awake] no portal inhibitor ({portal}); trying logind");
        match logind().await {
            Ok(fd) => Ok(Held::Logind(fd)),
            Err(error) => Err(format!("portal: {portal}; logind: {error}")),
        }
    }

    async fn portal() -> Result<Request<()>, ashpd::Error> {
        let proxy = InhibitProxy::new().await?;
        proxy
            .inhibit(
                None,
                InhibitFlags::Idle.into(),
                InhibitOptions::default().set_reason(super::REASON),
            )
            .await
    }

    async fn logind() -> Result<zbus::zvariant::OwnedFd, zbus::Error> {
        let connection = zbus::Connection::system().await?;
        let reply = connection
            .call_method(
                Some("org.freedesktop.login1"),
                "/org/freedesktop/login1",
                Some("org.freedesktop.login1.Manager"),
                "Inhibit",
                &("idle:sleep", "vdr", super::REASON, "block"),
            )
            .await?;
        reply.body().deserialize()
    }

    impl Drop for Held {
        fn drop(&mut self) {
            let Held::Portal(request) = self else { return };
            let Some(request) = request.take() else { return };
            match tokio::runtime::Handle::try_current() {
                Ok(handle) => {
                    handle.spawn(async move {
                        if let Err(error) = request.close().await {
                            tracing::warn!("[awake] could not close the portal inhibitor: {error}");
                        }
                    });
                }
                Err(_) => tracing::warn!("[awake] no runtime to close the portal inhibitor on"),
            }
        }
    }
}

/// Nothing to ask elsewhere yet.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod platform {
    pub const NAME: &str = "none";

    pub struct Held;

    pub async fn take() -> Result<Held, String> {
        Err("not supported on this platform".to_string())
    }
}

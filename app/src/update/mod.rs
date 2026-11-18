//! Finding, fetching and installing a newer release of vdr.
//!
//! **It asks first.** A check runs shortly after launch and once a day while
//! the setting allows it, and all a check does is say whether there is
//! something newer. Downloading and installing happen only when the person
//! presses the button, and never during a call: a call joined or being
//! joined makes the install step refuse, checked again on the worker just
//! before the swap and the restart (see [`set_call_active`]).
//!
//! **Signed.** Nothing downloaded is unpacked before its signature and
//! checksum pass: see `verify` for the chain and why an older signed
//! release cannot be replayed as a newer one.
//!
//! **Testing against a feed of your own.** A build made with
//! `VDR_UPDATE_TEST_PUBKEY=<64 hex>` in its environment trusts that key
//! instead of the release key, and only such a build reads the runtime
//! variable `VDR_UPDATE_FEED`: a URL answering with the same JSON as
//! GitHub's latest release, whose asset URLs may then be plain `http`.
//! Release builds set neither, so the override does not exist in them.
//!
//! **Quiet.** A check that fails (offline, rate-limited, a bad answer) is a
//! log line and a "could not check" in the Updates tab, never a popup.
//!
//! Pure logic lives in `release`, `verify` and the path checks in
//! `install`, where tests reach it; this file is the threads and the state
//! the interface is told about. Every message wakes the page and repaints
//! the window, so a [`Status`] is sent only when it differs from the last
//! one, and download progress moves in whole percent.

mod fetch;
mod install;
mod release;
mod verify;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use futures_channel::mpsc::UnboundedSender;

use install::Plan;
use release::{Platform, Release};

/// Long enough after launch that the check does not compete with the
/// camera, audio and endpoint all starting at once.
const FIRST_CHECK: Duration = Duration::from_secs(10);

/// How often an automatic check runs.
const EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// How often the schedule looks at the clock and the setting, so turning
/// the setting on is noticed without a restart.
const TICK: Duration = Duration::from_secs(60);

/// Whether a call is joined or on its way, as the call's own status says.
static CALL_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Told by the call whenever its status changes, and at the moment a call
/// is asked for, before its first status arrives. Process-wide rather than
/// a signal because the installer's worker thread must read it.
pub fn set_call_active(active: bool) {
    CALL_ACTIVE.store(active, Ordering::SeqCst);
}

fn call_active() -> bool {
    CALL_ACTIVE.load(Ordering::SeqCst)
}

/// What the updater is doing, for the Updates tab.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Phase {
    /// Nothing asked yet.
    #[default]
    Idle,
    Checking,
    UpToDate,
    /// The check failed; the reason is a sentence.
    CouldNotCheck(String),
    /// A newer release exists; see [`Status::offer`].
    Available,
    /// Whole percent of the package.
    Downloading(u8),
    Verifying,
    /// Downloaded and verified, waiting for the install (a call started).
    Ready,
    Installing,
    Restarting,
    /// Verified and left for the person to install by hand, at this path.
    Downloaded(PathBuf),
    /// Installed, but not restarted, because a call started meanwhile.
    Installed,
    Failed(String),
}

/// The release on offer.
#[derive(Clone, Debug, PartialEq)]
pub struct Offer {
    pub version: String,
    /// The release page, with its notes.
    pub page: String,
}

/// What this copy can do with an update, decided once at startup.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Ability {
    /// Not looked yet: the first moments after launch.
    #[default]
    Unknown,
    Install,
    /// Download and verify only; the reason it stops there.
    DownloadOnly(String),
    /// Only the release page; the reason.
    Refused(String),
}

/// Everything the interface shows about updates.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub phase: Phase,
    pub offer: Option<Offer>,
    pub ability: Ability,
}

impl Status {
    /// Whether there is a newer release to tell the person about: the dot
    /// on the settings button.
    pub fn has_update(&self) -> bool {
        self.offer.is_some() && !matches!(self.phase, Phase::Installed | Phase::Restarting)
    }
}

/// The handle the interface keeps. Cheap to clone; every action runs on a
/// thread of its own and reports through the channel it was started with.
#[derive(Clone)]
pub struct Updater(Arc<Inner>);

/// The same updater, for a component's props: there is only ever one.
impl PartialEq for Updater {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

struct Inner {
    reporter: Mutex<Reporter>,
    /// One check, download or install at a time.
    busy: AtomicBool,
    auto: AtomicBool,
    plan: OnceLock<Plan>,
    last_check: Mutex<Option<Instant>>,
    /// The newer release last found.
    found: Mutex<Option<Release>>,
    /// A verified package, and the release it is.
    ready: Mutex<Option<(Release, PathBuf)>>,
}

/// Sends a [`Status`] only when it changed.
struct Reporter {
    tx: UnboundedSender<Status>,
    last: Status,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic elsewhere while holding one of these leaves plain data, still
    // good to read; no reason to take the updater down with it.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Marks the updater busy for as long as it lives.
struct Busy<'a>(&'a AtomicBool);

impl<'a> Busy<'a> {
    fn take(flag: &'a AtomicBool) -> Option<Self> {
        flag.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| Self(flag))
    }
}

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl Updater {
    /// Starts the schedule. `auto` is the setting as saved.
    pub fn start(tx: UnboundedSender<Status>, auto: bool) -> Self {
        let updater = Self(Arc::new(Inner {
            reporter: Mutex::new(Reporter {
                tx,
                last: Status::default(),
            }),
            busy: AtomicBool::new(false),
            auto: AtomicBool::new(auto),
            plan: OnceLock::new(),
            last_check: Mutex::new(None),
            found: Mutex::new(None),
            ready: Mutex::new(None),
        }));
        let schedule = updater.clone();
        spawn("update-schedule", move || schedule.schedule());
        updater
    }

    /// Turns the automatic check on or off, from the next tick.
    pub fn set_auto(&self, on: bool) {
        self.0.auto.store(on, Ordering::SeqCst);
    }

    /// Checks now, unless something is already under way.
    pub fn check_now(&self) {
        let updater = self.clone();
        spawn("update-check", move || updater.check());
    }

    /// Downloads, verifies, installs and restarts, as far as this copy and
    /// the moment allow. The interface offers it only outside a call; the
    /// worker asks again before it changes anything.
    pub fn install(&self) {
        let updater = self.clone();
        spawn("update-install", move || updater.download_and_install());
    }

    fn report(&self, change: impl FnOnce(&mut Status)) {
        let mut reporter = lock(&self.0.reporter);
        let mut next = reporter.last.clone();
        change(&mut next);
        if next != reporter.last {
            // An error means the page is gone, and nobody is left to tell.
            let _ = reporter.tx.unbounded_send(next.clone());
            reporter.last = next;
        }
    }

    fn phase(&self) -> Phase {
        lock(&self.0.reporter).last.phase.clone()
    }

    fn plan(&self) -> &Plan {
        self.0.plan.get_or_init(install::plan)
    }

    fn schedule(&self) {
        let plan = self.plan().clone();
        tracing::info!("update: {plan:?}");
        install::cleanup(&plan);
        clean_downloads(matches!(plan, Plan::Swap(_)));
        self.report(|status| {
            status.ability = match plan {
                Plan::Swap(_) => Ability::Install,
                Plan::DownloadOnly(reason) => Ability::DownloadOnly(reason),
                Plan::Refused(reason) => Ability::Refused(reason),
            }
        });
        std::thread::sleep(FIRST_CHECK);
        loop {
            let due = lock(&self.0.last_check).is_none_or(|at| at.elapsed() >= EVERY);
            if due && self.0.auto.load(Ordering::SeqCst) {
                self.check();
            }
            std::thread::sleep(TICK);
        }
    }

    fn check(&self) {
        let Some(_busy) = Busy::take(&self.0.busy) else {
            tracing::debug!("update: busy; not checking");
            return;
        };
        *lock(&self.0.last_check) = Some(Instant::now());
        let before = self.phase();
        self.report(|status| status.phase = Phase::Checking);
        let result = block_on(async {
            let client = fetch::client()?;
            fetch::latest(&client).await
        });
        let release = match result {
            Ok(release) => release,
            Err(error) => {
                tracing::warn!("update: could not check: {error}");
                // An offer already made stands: a check that failed later
                // says nothing about it.
                let has_offer = lock(&self.0.found).is_some();
                self.report(|status| {
                    status.phase = if has_offer { before } else { Phase::CouldNotCheck(error) };
                });
                return;
            }
        };
        let running = release::running();
        if !release::is_update(&running, &release.version) {
            tracing::info!("update: {} is the latest; running {running}", release.tag);
            *lock(&self.0.found) = None;
            self.report(|status| {
                status.phase = Phase::UpToDate;
                status.offer = None;
            });
            return;
        }
        tracing::info!("update: {} is available; running {running}", release.tag);
        let ready = lock(&self.0.ready).as_ref().is_some_and(|(have, _)| have.tag == release.tag);
        let offer = Offer {
            version: release.version.to_string(),
            page: release.page.clone(),
        };
        *lock(&self.0.found) = Some(release);
        self.report(|status| {
            status.offer = Some(offer);
            status.phase = if ready { Phase::Ready } else { Phase::Available };
        });
    }

    fn download_and_install(&self) {
        let Some(_busy) = Busy::take(&self.0.busy) else {
            tracing::debug!("update: busy; not installing");
            return;
        };
        let Some(release) = lock(&self.0.found).clone() else { return };
        let plan = self.plan().clone();
        if let Plan::Refused(reason) = &plan {
            tracing::info!("update: not installing: {reason}");
            return;
        }
        let ready = lock(&self.0.ready)
            .as_ref()
            .filter(|(have, path)| have.tag == release.tag && path.is_file())
            .map(|(_, path)| path.clone());
        let path = match ready {
            Some(path) => path,
            None => match self.download(&release) {
                Ok(path) => {
                    *lock(&self.0.ready) = Some((release.clone(), path.clone()));
                    path
                }
                Err(error) => {
                    tracing::warn!("update: {error}");
                    self.report(|status| status.phase = Phase::Failed(error));
                    return;
                }
            },
        };
        let location = match plan {
            Plan::Swap(location) => location,
            Plan::DownloadOnly(reason) => {
                tracing::info!("update: {} downloaded and verified, not installed: {reason}", path.display());
                self.report(|status| status.phase = Phase::Downloaded(path));
                return;
            }
            Plan::Refused(_) => return,
        };
        if call_active() {
            tracing::info!("update: a call is on; the install waits");
            self.report(|status| status.phase = Phase::Ready);
            return;
        }
        self.report(|status| status.phase = Phase::Installing);
        if let Err(error) = install::install(&location, &path, &release.version, call_active) {
            tracing::warn!("update: install failed: {error}");
            self.report(|status| status.phase = Phase::Failed(capitalised(&error)));
            return;
        }
        tracing::info!("update: {} installed at {location:?}", release.tag);
        *lock(&self.0.ready) = None;
        clean_downloads(true);
        if call_active() {
            self.report(|status| status.phase = Phase::Installed);
            return;
        }
        self.report(|status| status.phase = Phase::Restarting);
        // Returns only if the restart failed: the new copy is in place, and
        // quitting and opening it again is all that is left.
        let error = install::relaunch(&location);
        tracing::warn!("update: could not restart: {error}");
        self.report(|status| status.phase = Phase::Installed);
    }

    /// Fetches the checksums and their signature, checks the signature,
    /// then fetches the package and checks it against them. Returns the
    /// package's path only if all of it passed.
    fn download(&self, release: &Release) -> Result<PathBuf, String> {
        let platform = Platform::current();
        let package = release::package(release, platform)?;
        let dir = updates_dir().ok_or("there is no data folder to download into")?;
        std::fs::create_dir_all(&dir).map_err(|error| format!("could not create {} ({error})", dir.display()))?;
        let key = verify::trusted_key();
        self.report(|status| status.phase = Phase::Downloading(0));
        let path = dir.join(&package.archive.name);
        let (sums, signature, digest) = block_on(async {
            let client = fetch::client()?;
            let sums = fetch::small(&client, &package.sums).await?;
            let signature = fetch::small(&client, &package.signature).await?;
            // Before the package, so a bad signature costs a few hundred
            // bytes rather than the whole download.
            verify::signed(&sums, &signature, &key).map_err(|refused| capitalised(&refused.to_string()))?;
            verify::listed(&sums, &package.archive.name).map_err(|refused| capitalised(&refused.to_string()))?;
            let digest = fetch::download(&client, &package.archive, &path, |done, total| {
                let percent = release::percent(done, total);
                self.report(|status| status.phase = Phase::Downloading(percent));
            })
            .await?;
            Ok::<_, String>((sums, signature, digest))
        })?;
        self.report(|status| status.phase = Phase::Verifying);
        let running = release::running();
        let claim = verify::Claim {
            tag: &release.tag,
            running: &running,
            platform,
            name: &package.archive.name,
            digest,
            sums: &sums,
            signature: &signature,
        };
        if let Err(refused) = verify::package(&claim, &key) {
            let _ = std::fs::remove_file(&path);
            return Err(capitalised(&refused.to_string()));
        }
        tracing::info!("update: {} verified", path.display());
        Ok(path)
    }
}

/// Runs a future to completion on the app's one runtime, from a thread of
/// our own (never a runtime worker), so reqwest's connection tasks have a
/// runtime to live on.
fn block_on<T>(future: impl std::future::Future<Output = Result<T, String>>) -> Result<T, String> {
    let runtime = crate::net::runtime::runtime().ok_or("the network runtime did not start")?;
    runtime.block_on(future)
}

fn spawn(name: &str, work: impl FnOnce() + Send + 'static) {
    if let Err(error) = std::thread::Builder::new().name(name.into()).spawn(work) {
        tracing::warn!("could not start the {name} thread: {error}");
    }
}

/// `updates/` in the data folder.
fn updates_dir() -> Option<PathBuf> {
    Some(crate::settings::data_dir()?.join("updates"))
}

/// Removes downloads: all of them once an install has used one, or where
/// this copy installs its own; otherwise only unfinished ones, since a
/// package left for the person to install by hand is theirs.
fn clean_downloads(all: bool) {
    let Some(dir) = updates_dir() else { return };
    if !dir.exists() {
        return;
    }
    if all {
        if let Err(error) = std::fs::remove_dir_all(&dir) {
            tracing::warn!("could not remove {}: {error}", dir.display());
        }
        return;
    }
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    for entry in entries.filter_map(Result::ok) {
        if entry.file_name().to_string_lossy().ends_with(".part") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// A sentence from the lower-case reasons the pieces give.
pub fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_changed_status_is_sent() {
        let (tx, mut rx) = futures_channel::mpsc::unbounded();
        let updater = Updater(Arc::new(Inner {
            reporter: Mutex::new(Reporter {
                tx,
                last: Status::default(),
            }),
            busy: AtomicBool::new(false),
            auto: AtomicBool::new(true),
            plan: OnceLock::new(),
            last_check: Mutex::new(None),
            found: Mutex::new(None),
            ready: Mutex::new(None),
        }));
        for percent in [0, 0, 0, 1, 1, 2, 2, 2] {
            updater.report(|status| status.phase = Phase::Downloading(percent));
        }
        let mut sent = Vec::new();
        while let Ok(status) = rx.try_recv() {
            sent.push(status.phase);
        }
        assert_eq!(sent, vec![Phase::Downloading(0), Phase::Downloading(1), Phase::Downloading(2)]);
    }

    #[test]
    fn only_one_job_at_a_time() {
        let flag = AtomicBool::new(false);
        let first = Busy::take(&flag);
        assert!(first.is_some());
        assert!(Busy::take(&flag).is_none());
        drop(first);
        assert!(Busy::take(&flag).is_some());
    }

    #[test]
    fn the_dot_shows_while_there_is_something_to_take() {
        let mut status = Status::default();
        assert!(!status.has_update());
        status.offer = Some(Offer {
            version: "0.2.0".into(),
            page: "https://example.invalid".into(),
        });
        status.phase = Phase::Available;
        assert!(status.has_update());
        status.phase = Phase::Downloading(40);
        assert!(status.has_update());
        status.phase = Phase::Installed;
        assert!(!status.has_update());
    }

    #[test]
    fn reasons_read_as_sentences() {
        assert_eq!(capitalised("the signature does not match"), "The signature does not match");
        assert_eq!(capitalised(""), "");
    }
}

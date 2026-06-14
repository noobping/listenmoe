use super::logic::{cached_download_matches_size, ReleaseAsset, ReleaseCandidate, SelectedRelease};
use super::windows as platform;
use super::UpdateUi;
use crate::locale::gettext;
use crate::log::{is_verbose, now_string};
use adw::gio::SimpleAction;
use adw::glib;
use adw::gtk::ApplicationWindow;
use adw::prelude::*;
use adw::Application;
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, USER_AGENT};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::cell::{Cell, RefCell};
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::mpsc::TryRecvError;
use std::sync::Arc;
use std::time::Duration;

const GITHUB_API_ACCEPT: &str = "application/vnd.github+json";
const GITHUB_API_VERSION: &str = "2022-11-28";
const GITHUB_RELEASES_PER_PAGE: usize = 100;
const WORKER_POLL_INTERVAL_MS: u64 = 50;
const TEMPORARY_STATUS_MS: u64 = 3_000;

#[derive(Clone)]
pub(crate) struct UpdaterController {
    inner: Rc<UpdaterControllerInner>,
}

struct UpdaterControllerInner {
    app: Application,
    ui: UpdateUi,
    state: RefCell<UpdateState>,
    check_action: SimpleAction,
    cancel_action: SimpleAction,
    auto_check_started: Cell<bool>,
    next_run_id: Cell<u64>,
    status_generation: Cell<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DownloadedUpdate {
    pub(super) path: PathBuf,
    pub(super) size: u64,
}

#[derive(Clone)]
enum UpdateState {
    Idle,
    Checking {
        mode: CheckMode,
        run_id: u64,
    },
    Downloading {
        mode: CheckMode,
        run_id: u64,
        release: SelectedRelease,
        download: DownloadedUpdate,
        cancel: Arc<AtomicBool>,
        downloaded: u64,
    },
    Ready {
        release: SelectedRelease,
        download: DownloadedUpdate,
    },
    Installing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CheckMode {
    Automatic,
    Manual,
}

#[derive(Clone, Debug)]
enum WorkerMessage {
    CheckFinished {
        run_id: u64,
        result: Result<Option<SelectedRelease>, String>,
    },
    DownloadProgress {
        run_id: u64,
        downloaded: u64,
    },
    DownloadReady {
        run_id: u64,
        download: DownloadedUpdate,
    },
    DownloadCancelled {
        run_id: u64,
    },
    DownloadFailed {
        run_id: u64,
        error: String,
    },
}

#[derive(Deserialize)]
struct GitHubReleaseResponse {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<GitHubAssetResponse>,
}

#[derive(Deserialize)]
struct GitHubAssetResponse {
    name: String,
    browser_download_url: String,
    size: u64,
    digest: Option<String>,
}

impl UpdaterController {
    fn new(app: &Application, ui: UpdateUi) -> Self {
        Self {
            inner: Rc::new(UpdaterControllerInner {
                app: app.clone(),
                ui,
                state: RefCell::new(UpdateState::Idle),
                check_action: SimpleAction::new("check-for-updates", None),
                cancel_action: SimpleAction::new("cancel-update", None),
                auto_check_started: Cell::new(false),
                next_run_id: Cell::new(0),
                status_generation: Cell::new(0),
            }),
        }
    }

    pub(crate) fn after_window_presented(&self) {
        if self.inner.auto_check_started.replace(true) {
            return;
        }

        self.start_check(CheckMode::Automatic);
    }

    pub(crate) fn shutdown(&self) {
        match self.inner.state.borrow().clone() {
            UpdateState::Downloading {
                cancel, download, ..
            } => {
                cancel.store(true, Ordering::Relaxed);
                platform::cleanup_download(&download);
            }
            UpdateState::Ready { download, .. } => platform::cleanup_download(&download),
            UpdateState::Idle | UpdateState::Checking { .. } | UpdateState::Installing => {}
        }
    }

    fn install_actions(&self, window: &ApplicationWindow) {
        {
            let controller = self.clone();
            self.inner
                .check_action
                .connect_activate(move |_, _| controller.start_check(CheckMode::Manual));
        }
        window.add_action(&self.inner.check_action);

        {
            let controller = self.clone();
            self.inner
                .cancel_action
                .connect_activate(move |_, _| controller.cancel_update());
        }
        window.add_action(&self.inner.cancel_action);
        self.set_update_menu_options(true, false);

        {
            let controller = self.clone();
            self.inner
                .ui
                .update_button
                .connect_clicked(move |_| controller.begin_install_flow());
        }
    }

    fn start_check(&self, mode: CheckMode) {
        if !platform::supports_updater() {
            return;
        }

        {
            let mut state = self.inner.state.borrow_mut();
            match &mut *state {
                UpdateState::Idle => {}
                UpdateState::Checking {
                    mode: existing_mode,
                    ..
                } => {
                    if matches!(mode, CheckMode::Manual) {
                        *existing_mode = CheckMode::Manual;
                        self.present_checking();
                    }
                    return;
                }
                UpdateState::Downloading {
                    mode: existing_mode,
                    release,
                    downloaded,
                    ..
                } => {
                    if matches!(mode, CheckMode::Manual) {
                        *existing_mode = CheckMode::Manual;
                        self.present_download(release, *downloaded);
                    }
                    return;
                }
                UpdateState::Ready { release, download } => {
                    if cached_download_matches_release(download, &release.asset) {
                        self.present_ready(release);
                        return;
                    }

                    platform::cleanup_download(download);
                    *state = UpdateState::Idle;
                }
                UpdateState::Installing => return,
            }
        }

        let run_id = self.next_run_id();
        *self.inner.state.borrow_mut() = UpdateState::Checking { mode, run_id };
        self.set_update_menu_options(false, matches!(mode, CheckMode::Manual));
        if matches!(mode, CheckMode::Manual) {
            self.present_checking();
        }

        let (tx, rx) = mpsc::channel();
        if let Err(error) = spawn_worker("updater-check", move || {
            let result = fetch_update_release();
            let _ = tx.send(WorkerMessage::CheckFinished { run_id, result });
        }) {
            log_error(format!("Failed to spawn update check worker: {error}"));
            *self.inner.state.borrow_mut() = UpdateState::Idle;
            self.set_update_menu_options(true, false);
            if matches!(mode, CheckMode::Manual) {
                self.present_temporary_status(
                    gettext("Couldn't check for updates"),
                    gettext("Try again later."),
                );
            }
            return;
        }

        let controller = self.clone();
        poll_worker(rx, move |message| controller.handle_worker_message(message));
    }

    fn handle_worker_message(&self, message: WorkerMessage) {
        match message {
            WorkerMessage::CheckFinished { run_id, result } => {
                self.handle_check_finished(run_id, result)
            }
            WorkerMessage::DownloadProgress { run_id, downloaded } => {
                self.handle_download_progress(run_id, downloaded);
            }
            WorkerMessage::DownloadReady { run_id, download } => {
                self.handle_download_ready(run_id, download);
            }
            WorkerMessage::DownloadCancelled { run_id } => self.handle_download_cancelled(run_id),
            WorkerMessage::DownloadFailed { run_id, error } => {
                self.handle_download_failed(run_id, &error);
            }
        }
    }

    fn handle_check_finished(&self, run_id: u64, result: Result<Option<SelectedRelease>, String>) {
        let state = self.inner.state.borrow().clone();
        let UpdateState::Checking {
            run_id: current_run_id,
            mode,
        } = state
        else {
            return;
        };
        if current_run_id != run_id {
            return;
        }

        match result {
            Ok(Some(release)) => self.start_download(run_id, mode, release),
            Ok(None) => {
                *self.inner.state.borrow_mut() = UpdateState::Idle;
                if matches!(mode, CheckMode::Manual) {
                    self.present_temporary_status(
                        gettext("Already up to date"),
                        gettext("You have the latest version of Listen Moe."),
                    );
                } else {
                    self.hide_update_ui();
                }
            }
            Err(error) => {
                log_error(format!("Failed to check for updates: {error}"));
                *self.inner.state.borrow_mut() = UpdateState::Idle;
                if matches!(mode, CheckMode::Manual) {
                    self.present_temporary_status(
                        gettext("Couldn't check for updates"),
                        gettext("Try again later."),
                    );
                } else {
                    self.hide_update_ui();
                }
            }
        }
    }

    fn start_download(&self, run_id: u64, mode: CheckMode, release: SelectedRelease) {
        if let Err(error) = validate_release_asset_digest(&release.asset) {
            log_error(format!(
                "Refusing to download update {}: {error}",
                release.version
            ));
            *self.inner.state.borrow_mut() = UpdateState::Idle;
            self.set_update_menu_options(true, false);
            if matches!(mode, CheckMode::Manual) {
                self.present_temporary_status(
                    gettext("Couldn't check for updates"),
                    gettext("The release metadata is missing a valid SHA-256 digest."),
                );
            }
            return;
        }

        let download = platform::download_target(&release);
        if cached_download_matches_release(&download, &release.asset) {
            log_info(format!(
                "Reusing cached update download for version {}.",
                release.version
            ));
            *self.inner.state.borrow_mut() = UpdateState::Ready {
                release: release.clone(),
                download,
            };
            self.handle_ready_update(&release);
            return;
        }

        let cancel = Arc::new(AtomicBool::new(false));
        *self.inner.state.borrow_mut() = UpdateState::Downloading {
            mode,
            run_id,
            release: release.clone(),
            download: download.clone(),
            cancel: cancel.clone(),
            downloaded: 0,
        };
        self.set_update_menu_options(false, true);
        self.present_download(&release, 0);

        let (tx, rx) = mpsc::channel();
        if let Err(error) = spawn_worker("updater-download", move || {
            let result = download_release_asset(run_id, &release, &download, &cancel, &tx);
            if let Some(message) = result {
                let _ = tx.send(message);
            }
        }) {
            log_error(format!("Failed to spawn update download worker: {error}"));
            *self.inner.state.borrow_mut() = UpdateState::Idle;
            self.set_update_menu_options(true, false);
            if matches!(mode, CheckMode::Manual) {
                self.present_temporary_status(
                    gettext("Couldn't download the update"),
                    gettext("Try again later."),
                );
            } else {
                self.hide_update_ui();
            }
            return;
        }

        let controller = self.clone();
        poll_worker(rx, move |message| controller.handle_worker_message(message));
    }

    fn handle_download_progress(&self, run_id: u64, downloaded: u64) {
        let mut state = self.inner.state.borrow_mut();
        let UpdateState::Downloading {
            run_id: current_run_id,
            release,
            downloaded: current_downloaded,
            ..
        } = &mut *state
        else {
            return;
        };
        if *current_run_id != run_id {
            return;
        }

        *current_downloaded = downloaded;
        self.update_download_progress(release, downloaded);
    }

    fn handle_download_ready(&self, run_id: u64, download: DownloadedUpdate) {
        let state = self.inner.state.borrow().clone();
        let UpdateState::Downloading {
            run_id: current_run_id,
            release,
            ..
        } = state
        else {
            return;
        };
        if current_run_id != run_id {
            return;
        }

        *self.inner.state.borrow_mut() = UpdateState::Ready {
            release: release.clone(),
            download,
        };
        self.handle_ready_update(&release);
    }

    fn handle_download_cancelled(&self, run_id: u64) {
        let state = self.inner.state.borrow().clone();
        let UpdateState::Downloading {
            run_id: current_run_id,
            download,
            ..
        } = state
        else {
            return;
        };
        if current_run_id != run_id {
            return;
        }

        platform::cleanup_download(&download);
        *self.inner.state.borrow_mut() = UpdateState::Idle;
        self.present_temporary_status(
            gettext("Update canceled"),
            gettext("The update was canceled."),
        );
    }

    fn handle_download_failed(&self, run_id: u64, error: &str) {
        let state = self.inner.state.borrow().clone();
        let UpdateState::Downloading {
            run_id: current_run_id,
            download,
            ..
        } = state
        else {
            return;
        };
        if current_run_id != run_id {
            return;
        }

        log_error(format!("Failed to download the update: {error}"));
        platform::cleanup_download(&download);
        *self.inner.state.borrow_mut() = UpdateState::Idle;
        self.present_temporary_status(
            gettext("Couldn't download the update"),
            gettext("Try again later."),
        );
    }

    fn cancel_update(&self) {
        match self.inner.state.borrow().clone() {
            UpdateState::Checking { .. } => {
                *self.inner.state.borrow_mut() = UpdateState::Idle;
                self.present_temporary_status(
                    gettext("Update canceled"),
                    gettext("The update check was canceled."),
                );
            }
            UpdateState::Downloading {
                cancel, download, ..
            } => {
                cancel.store(true, Ordering::Relaxed);
                platform::cleanup_download(&download);
                *self.inner.state.borrow_mut() = UpdateState::Idle;
                self.present_temporary_status(
                    gettext("Update canceled"),
                    gettext("The update was canceled."),
                );
            }
            UpdateState::Ready { download, .. } => {
                platform::cleanup_download(&download);
                *self.inner.state.borrow_mut() = UpdateState::Idle;
                self.present_temporary_status(
                    gettext("Update canceled"),
                    gettext("The downloaded installer will not be installed."),
                );
            }
            UpdateState::Idle | UpdateState::Installing => {}
        }
    }

    fn begin_install_flow(&self) {
        let state = self.inner.state.borrow().clone();
        let UpdateState::Ready { release, download } = state else {
            return;
        };

        if !cached_download_matches_release(&download, &release.asset) {
            platform::cleanup_download(&download);
            *self.inner.state.borrow_mut() = UpdateState::Idle;
            self.start_check(CheckMode::Manual);
            return;
        }

        self.launch_update(&download);
    }

    fn launch_update(&self, download: &DownloadedUpdate) {
        match platform::launch_update(download) {
            Ok(()) => {
                *self.inner.state.borrow_mut() = UpdateState::Installing;
                self.set_update_menu_options(false, false);
                self.set_update_progress(Some(1.0));
                self.set_update_title(
                    gettext("Installing update"),
                    gettext("Listen Moe will close."),
                );
                self.inner.app.quit();
            }
            Err(error) => {
                log_error(format!("Failed to start update install: {error}"));
                self.present_temporary_status(
                    gettext("Couldn't install the update"),
                    gettext(platform::install_failed_description()),
                );
            }
        }
    }

    fn present_checking(&self) {
        self.show_update_ui();
        self.set_update_progress(Some(0.0));
        self.inner
            .ui
            .update_button
            .set_tooltip_text(Some(&gettext("Checking for updates")));
        self.set_update_title(
            gettext("Checking for updates"),
            gettext(platform::update_check_body()),
        );
    }

    fn present_download(&self, release: &SelectedRelease, downloaded: u64) {
        self.show_update_ui();
        self.inner
            .ui
            .update_button
            .set_tooltip_text(Some(&gettext("Downloading update")));
        self.update_download_progress(release, downloaded);
    }

    fn update_download_progress(&self, release: &SelectedRelease, downloaded: u64) {
        let total = release.asset.size;
        let fraction = if total > 0 {
            Some((downloaded.min(total) as f64) / (total as f64))
        } else {
            Some(0.0)
        };
        self.set_update_progress(fraction);
        self.set_update_title(
            gettext("Downloading update"),
            format!(
                "{} {} - {}",
                gettext(platform::update_available_description()),
                release.version,
                download_status_label(downloaded, total)
            ),
        );
    }

    fn present_ready(&self, release: &SelectedRelease) {
        self.show_update_ui();
        self.set_update_progress(Some(1.0));
        self.inner
            .ui
            .update_button
            .set_tooltip_text(Some(&gettext("Install update")));
        self.set_update_title(
            gettext("Update ready"),
            format!(
                "{} {} - {}",
                gettext("Version"),
                release.version,
                gettext(platform::ready_status())
            ),
        );
    }

    fn handle_ready_update(&self, release: &SelectedRelease) {
        self.present_ready(release);
        self.begin_install_flow();
    }

    fn present_temporary_status(&self, title: String, subtitle: String) {
        self.restore_transport_controls();
        self.inner.ui.update_title_override.set(true);
        self.set_update_menu_options(true, false);
        self.inner
            .status_generation
            .set(self.next_status_generation());
        let generation = self.inner.status_generation.get();
        self.set_update_title(title, subtitle);

        let controller = self.clone();
        glib::timeout_add_local_once(Duration::from_millis(TEMPORARY_STATUS_MS), move || {
            if controller.inner.status_generation.get() != generation {
                return;
            }
            if controller.inner.ui.update_active.get() {
                return;
            }
            controller.inner.ui.update_title_override.set(false);
            controller.restore_normal_title();
        });
    }

    fn show_update_ui(&self) {
        self.inner
            .status_generation
            .set(self.next_status_generation());
        self.inner.ui.update_active.set(true);
        self.inner.ui.update_title_override.set(true);
        self.inner.ui.play_button.set_visible(false);
        self.inner.ui.pause_button.set_visible(false);
        self.inner.ui.update_button.set_visible(true);
        self.set_update_menu_options(false, true);
    }

    fn hide_update_ui(&self) {
        self.restore_transport_controls();
        self.set_update_menu_options(true, false);
        self.inner.ui.update_title_override.set(false);
        self.restore_normal_title();
    }

    fn restore_transport_controls(&self) {
        self.inner.ui.update_active.set(false);
        self.inner.ui.update_button.set_visible(false);
        if self.inner.ui.playback_playing.get() {
            self.inner.ui.play_button.set_visible(false);
            self.inner.ui.pause_button.set_visible(true);
        } else {
            self.inner.ui.pause_button.set_visible(false);
            self.inner.ui.play_button.set_visible(true);
        }
    }

    fn restore_normal_title(&self) {
        let (title, subtitle) = self.inner.ui.normal_title.borrow().clone();
        self.inner.ui.win_title.set_title(&title);
        self.inner.ui.win_title.set_subtitle(&subtitle);
    }

    fn set_update_title(&self, title: String, subtitle: String) {
        self.inner.ui.win_title.set_title(&title);
        self.inner.ui.win_title.set_subtitle(&subtitle);
    }

    fn set_update_progress(&self, fraction: Option<f64>) {
        self.inner
            .ui
            .update_progress
            .set(fraction.map(|fraction| fraction.clamp(0.0, 1.0)));
        self.inner.ui.update_progress_area.queue_draw();
    }

    fn set_update_menu_options(&self, check_enabled: bool, cancel_enabled: bool) {
        self.inner.check_action.set_enabled(check_enabled);
        self.inner.cancel_action.set_enabled(cancel_enabled);
    }

    fn next_run_id(&self) -> u64 {
        let next = self.inner.next_run_id.get().saturating_add(1);
        self.inner.next_run_id.set(next);
        next
    }

    fn next_status_generation(&self) -> u64 {
        self.inner.status_generation.get().saturating_add(1)
    }
}

pub(crate) fn register_window(
    app: &Application,
    window: &ApplicationWindow,
    ui: UpdateUi,
) -> Option<UpdaterController> {
    if !platform::supports_updater() {
        return None;
    }

    let controller = UpdaterController::new(app, ui);
    controller.install_actions(window);
    Some(controller)
}

pub(crate) fn handle_special_command(_args: &[std::ffi::OsString]) -> Option<ExitCode> {
    None
}

pub(super) fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            _ => ch,
        })
        .collect()
}

fn repository_owner_and_name() -> Result<(&'static str, &'static str), String> {
    let repository = env!("CARGO_PKG_REPOSITORY");
    let path = repository
        .strip_prefix("https://github.com/")
        .or_else(|| repository.strip_prefix("http://github.com/"))
        .ok_or_else(|| format!("Unsupported repository URL for updates: {repository}"))?;
    let mut parts = path.split('/');
    let owner = parts
        .next()
        .filter(|part| !part.is_empty())
        .ok_or_else(|| format!("Missing owner in repository URL: {repository}"))?;
    let repo = parts
        .next()
        .filter(|part| !part.is_empty())
        .map(|part| part.trim_end_matches(".git"))
        .ok_or_else(|| format!("Missing repository name in repository URL: {repository}"))?;
    Ok((owner, repo))
}

fn fetch_update_release() -> Result<Option<SelectedRelease>, String> {
    let (owner, repo) = repository_owner_and_name()?;
    let url = format!(
        "https://api.github.com/repos/{owner}/{repo}/releases?per_page={GITHUB_RELEASES_PER_PAGE}"
    );

    let raw = github_http_client()?
        .get(url)
        .send()
        .map_err(http_error("send GitHub release request"))?
        .error_for_status()
        .map_err(http_error("read GitHub release response"))?
        .text()
        .map_err(http_error("read GitHub release body"))?;

    let releases = serde_json::from_str::<Vec<GitHubReleaseResponse>>(&raw)
        .map_err(|error| format!("Failed to decode GitHub release response: {error}"))?
        .into_iter()
        .map(|release| ReleaseCandidate {
            tag_name: release.tag_name,
            draft: release.draft,
            prerelease: release.prerelease,
            assets: release
                .assets
                .into_iter()
                .map(|asset| ReleaseAsset {
                    name: asset.name,
                    browser_download_url: asset.browser_download_url,
                    size: asset.size,
                    sha256_digest: asset.digest,
                })
                .collect(),
        })
        .collect::<Vec<_>>();

    platform::select_update_release(env!("CARGO_PKG_VERSION"), &releases)
}

fn download_release_asset(
    run_id: u64,
    release: &SelectedRelease,
    download: &DownloadedUpdate,
    cancel: &Arc<AtomicBool>,
    tx: &mpsc::Sender<WorkerMessage>,
) -> Option<WorkerMessage> {
    match perform_download(release, download, cancel, tx, run_id) {
        Ok(()) => Some(WorkerMessage::DownloadReady {
            run_id,
            download: download.clone(),
        }),
        Err(DownloadFailure::Cancelled) => Some(WorkerMessage::DownloadCancelled { run_id }),
        Err(DownloadFailure::Error(error)) => Some(WorkerMessage::DownloadFailed { run_id, error }),
    }
}

fn perform_download(
    release: &SelectedRelease,
    download: &DownloadedUpdate,
    cancel: &Arc<AtomicBool>,
    tx: &mpsc::Sender<WorkerMessage>,
    run_id: u64,
) -> Result<(), DownloadFailure> {
    let Some(parent) = download.path.parent() else {
        return Err(DownloadFailure::Error(
            "Update download path has no parent directory.".to_string(),
        ));
    };
    fs::create_dir_all(parent).map_err(download_fs_error("create update download directory"))?;

    if download.path.exists() && !cached_download_matches_release(download, &release.asset) {
        fs::remove_file(&download.path).map_err(download_fs_error("remove stale update file"))?;
    }

    let temp_path = download.path.with_extension("download");
    if temp_path.exists() {
        fs::remove_file(&temp_path)
            .map_err(download_fs_error("remove stale partial update file"))?;
    }

    let mut response = asset_download_client()?
        .get(&release.asset.browser_download_url)
        .send()
        .map_err(download_http_error("send release asset request"))?
        .error_for_status()
        .map_err(download_http_error("download release asset"))?;

    let mut file =
        File::create(&temp_path).map_err(download_fs_error("create partial update file"))?;
    let mut downloaded = 0u64;
    let mut buffer = [0u8; 64 * 1024];

    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = fs::remove_file(&temp_path);
            return Err(DownloadFailure::Cancelled);
        }

        let read = response
            .read(&mut buffer)
            .map_err(download_io_error("read update bytes"))?;
        if read == 0 {
            break;
        }

        file.write_all(&buffer[..read])
            .map_err(download_io_error("write update bytes"))?;
        downloaded = downloaded.saturating_add(read as u64);
        let _ = tx.send(WorkerMessage::DownloadProgress { run_id, downloaded });
    }

    file.flush()
        .map_err(download_io_error("flush partial update file"))?;
    drop(file);

    if cancel.load(Ordering::Relaxed) {
        let _ = fs::remove_file(&temp_path);
        return Err(DownloadFailure::Cancelled);
    }

    if download.size > 0 && downloaded != download.size {
        let _ = fs::remove_file(&temp_path);
        return Err(DownloadFailure::Error(format!(
            "Update size mismatch after download (expected {}, got {}).",
            download.size, downloaded
        )));
    }

    if let Err(error) = validate_downloaded_update(&temp_path, download, &release.asset) {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }

    fs::rename(&temp_path, &download.path).map_err(download_fs_error("finalize update file"))?;
    Ok(())
}

fn github_http_client() -> Result<Client, String> {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static(GITHUB_API_ACCEPT));
    headers.insert(
        "X-GitHub-Api-Version",
        HeaderValue::from_static(GITHUB_API_VERSION),
    );
    headers.insert(
        USER_AGENT,
        HeaderValue::from_static(concat!(
            env!("CARGO_PKG_NAME"),
            "/",
            env!("CARGO_PKG_VERSION")
        )),
    );

    Client::builder()
        .default_headers(headers)
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(http_error("build GitHub client"))
}

fn asset_download_client() -> Result<Client, DownloadFailure> {
    Client::builder()
        .user_agent(concat!(
            env!("CARGO_PKG_NAME"),
            "/",
            env!("CARGO_PKG_VERSION")
        ))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(download_http_error("build download client"))
}

fn cached_download_matches_release(download: &DownloadedUpdate, asset: &ReleaseAsset) -> bool {
    validate_update_file(&download.path, download.size, asset).is_ok()
}

fn validate_downloaded_update(
    path: &Path,
    download: &DownloadedUpdate,
    asset: &ReleaseAsset,
) -> Result<(), DownloadFailure> {
    validate_update_file(path, download.size, asset).map_err(DownloadFailure::Error)
}

fn validate_update_file(
    path: &Path,
    expected_size: u64,
    asset: &ReleaseAsset,
) -> Result<(), String> {
    if !cached_download_matches_size(path, expected_size) {
        return Err(format!("Update size mismatch for '{}'.", path.display()));
    }

    let expected_digest = parse_release_sha256_digest(asset)?;
    let actual_digest = sha256_file_hex(path)
        .map_err(|error| format!("Failed to hash update file '{}': {error}", path.display()))?;
    if !actual_digest.eq_ignore_ascii_case(expected_digest) {
        return Err(format!("Update SHA-256 mismatch for '{}'.", path.display()));
    }

    Ok(())
}

fn validate_release_asset_digest(asset: &ReleaseAsset) -> Result<(), String> {
    parse_release_sha256_digest(asset).map(|_| ())
}

fn parse_release_sha256_digest(asset: &ReleaseAsset) -> Result<&str, String> {
    let digest = asset.sha256_digest.as_deref().ok_or_else(|| {
        format!(
            "Release asset '{}' is missing a GitHub SHA-256 digest.",
            asset.name
        )
    })?;
    let digest = digest.trim();
    let Some(hex) = digest.strip_prefix("sha256:") else {
        return Err(format!(
            "Release asset '{}' has an unsupported digest format.",
            asset.name
        ));
    };

    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!(
            "Release asset '{}' has an invalid SHA-256 digest.",
            asset.name
        ));
    }

    Ok(hex)
}

fn sha256_file_hex(path: &Path) -> Result<String, std::io::Error> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];

    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(&mut hex, "{byte:02x}");
    }
    Ok(hex)
}

fn download_status_label(downloaded: u64, total: u64) -> String {
    if total == 0 {
        return format!("{} {}", gettext("Downloaded"), format_bytes(downloaded));
    }

    let percentage = ((downloaded.min(total) as f64) / (total as f64)) * 100.0;
    format!(
        "{} of {} ({percentage:.0}%)",
        format_bytes(downloaded),
        format_bytes(total),
    )
}

fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;

    if bytes as f64 >= MIB {
        format!("{:.1} MiB", (bytes as f64) / MIB)
    } else if bytes as f64 >= KIB {
        format!("{:.1} KiB", (bytes as f64) / KIB)
    } else {
        format!("{bytes} B")
    }
}

fn poll_worker<T: Send + 'static>(
    rx: mpsc::Receiver<T>,
    mut handle_message: impl FnMut(T) + 'static,
) {
    glib::timeout_add_local(Duration::from_millis(WORKER_POLL_INTERVAL_MS), move || {
        loop {
            match rx.try_recv() {
                Ok(message) => handle_message(message),
                Err(TryRecvError::Empty) => return glib::ControlFlow::Continue,
                Err(TryRecvError::Disconnected) => return glib::ControlFlow::Break,
            }
        }
    });
}

fn spawn_worker(name: &'static str, f: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(f)?;
    Ok(())
}

fn log_info(message: String) {
    if is_verbose() {
        println!("[{}] {message}", now_string());
    }
}

fn log_error(message: String) {
    eprintln!("[{}] {message}", now_string());
}

enum DownloadFailure {
    Cancelled,
    Error(String),
}

fn http_error(context: &'static str) -> impl FnOnce(reqwest::Error) -> String {
    move |error| format!("Failed to {context}: {error}")
}

fn download_http_error(context: &'static str) -> impl FnOnce(reqwest::Error) -> DownloadFailure {
    move |error| DownloadFailure::Error(format!("Failed to {context}: {error}"))
}

fn download_fs_error(context: &'static str) -> impl FnOnce(std::io::Error) -> DownloadFailure {
    move |error| DownloadFailure::Error(format!("Failed to {context}: {error}"))
}

fn download_io_error(context: &'static str) -> impl FnOnce(std::io::Error) -> DownloadFailure {
    move |error| DownloadFailure::Error(format!("Failed to {context}: {error}"))
}

use super::common::{sanitize_filename, DownloadedUpdate};
use super::logic::{
    select_update_release as select_windows_update_release, ReleaseCandidate, SelectedRelease,
};
use std::path::PathBuf;

pub(super) fn supports_updater() -> bool {
    true
}

pub(super) fn update_check_body() -> &'static str {
    "Looking for a newer Windows installer on GitHub Releases."
}

pub(super) fn update_available_description() -> &'static str {
    "A newer Windows release is available."
}

pub(super) fn ready_status() -> &'static str {
    "The installer is ready to run."
}

pub(super) fn install_failed_description() -> &'static str {
    "Couldn't start the installer."
}

pub(super) fn select_update_release(
    current_version: &str,
    releases: &[ReleaseCandidate],
) -> Result<Option<SelectedRelease>, String> {
    Ok(select_windows_update_release(current_version, releases))
}

pub(super) fn download_target(release: &SelectedRelease) -> DownloadedUpdate {
    DownloadedUpdate {
        path: cached_download_path(release),
        size: release.asset.size,
    }
}

pub(super) fn cleanup_download(_download: &DownloadedUpdate) {}

pub(super) fn launch_update(download: &DownloadedUpdate) -> Result<(), String> {
    std::process::Command::new("msiexec")
        .arg("/i")
        .arg(&download.path)
        .arg("/quiet")
        .arg("/norestart")
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Failed to start msiexec for update install: {error}"))
}

fn cached_download_path(release: &SelectedRelease) -> PathBuf {
    let base = dirs_next::cache_dir()
        .or_else(dirs_next::data_local_dir)
        .unwrap_or_else(std::env::temp_dir);
    base.join(env!("CARGO_PKG_NAME"))
        .join("updates")
        .join(format!(
            "{}-{}",
            release.version,
            sanitize_filename(&release.asset.name)
        ))
}

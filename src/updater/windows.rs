use super::common::{sanitize_filename, DownloadedUpdate};
use super::logic::{
    select_update_release as select_windows_update_release, ReleaseCandidate, SelectedRelease,
};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_INVALID_PARAMETER, WAIT_FAILED, WAIT_OBJECT_0,
};
use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
use windows_sys::Win32::System::Threading::{
    OpenProcess, WaitForSingleObject, INFINITE, PROCESS_SYNCHRONIZE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

const INSTALL_UPDATE_COMMAND: &str = "--listenmoe-install-update";

#[derive(Debug, PartialEq, Eq)]
struct InstallUpdateArgs {
    parent_pid: u32,
    installer_path: PathBuf,
    log_path: PathBuf,
}

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
    let executable = std::env::current_exe()
        .map_err(|error| format!("Failed to locate the updater executable: {error}"))?;

    let mut helper = Command::new(executable);
    helper
        .arg(INSTALL_UPDATE_COMMAND)
        .arg(std::process::id().to_string())
        .arg(&download.path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(download_directory) = download.path.parent() {
        helper.current_dir(download_directory);
    }

    helper
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Failed to start the update helper: {error}"))
}

pub(super) fn handle_special_command(args: &[OsString]) -> Option<Result<(), String>> {
    let parsed = parse_install_update_args(args)?;
    let result = parsed.and_then(run_install_update);

    if let Err(error) = &result {
        show_update_error(error);
    }

    Some(result)
}

fn parse_install_update_args(args: &[OsString]) -> Option<Result<InstallUpdateArgs, String>> {
    if args.get(1).map(OsString::as_os_str) != Some(OsStr::new(INSTALL_UPDATE_COMMAND)) {
        return None;
    }

    Some((|| {
        if args.len() != 4 {
            return Err("The update helper received invalid arguments.".to_string());
        }

        let parent_pid = args[2]
            .to_str()
            .ok_or_else(|| "The update helper received an invalid process ID.".to_string())?
            .parse::<u32>()
            .ok()
            .filter(|pid| *pid != 0)
            .ok_or_else(|| "The update helper received an invalid process ID.".to_string())?;

        let installer_path = PathBuf::from(&args[3]);
        let is_msi = installer_path
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|extension| extension.eq_ignore_ascii_case("msi"));
        if !installer_path.is_absolute() || !is_msi {
            return Err("The update helper received an invalid installer path.".to_string());
        }

        let log_path = installer_path.with_extension("install.log");
        Ok(InstallUpdateArgs {
            parent_pid,
            installer_path,
            log_path,
        })
    })())
}

fn run_install_update(args: InstallUpdateArgs) -> Result<(), String> {
    wait_for_parent(args.parent_pid)?;

    let metadata = fs::metadata(&args.installer_path).map_err(|error| {
        format!(
            "Failed to access the downloaded installer '{}': {error}",
            args.installer_path.display()
        )
    })?;
    if !metadata.is_file() {
        return Err(format!(
            "The downloaded installer '{}' is not a file.",
            args.installer_path.display()
        ));
    }

    // Do not wait here: this helper is the installed application executable, so it
    // must exit before Windows Installer replaces the application's files.
    let mut installer = Command::new(windows_installer_path()?);
    installer
        .arg("/i")
        .arg(&args.installer_path)
        .arg("/qb")
        .arg("/norestart")
        .arg("/L*V")
        .arg(&args.log_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(download_directory) = args.installer_path.parent() {
        installer.current_dir(download_directory);
    }

    installer
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Failed to start Windows Installer: {error}"))
}

fn windows_installer_path() -> Result<PathBuf, String> {
    // Windows paths can exceed MAX_PATH when long-path support is enabled.
    let mut system_directory = vec![0u16; 32_768];
    // SAFETY: the buffer is writable for the supplied length and remains alive for
    // the duration of the call.
    let length = unsafe {
        GetSystemDirectoryW(system_directory.as_mut_ptr(), system_directory.len() as u32)
    } as usize;
    if length == 0 {
        return Err(format!(
            "Failed to locate Windows Installer: {}",
            std::io::Error::last_os_error()
        ));
    }
    if length >= system_directory.len() {
        return Err("The Windows system directory path is unexpectedly long.".to_string());
    }

    system_directory.truncate(length);
    Ok(PathBuf::from(OsString::from_wide(&system_directory)).join("msiexec.exe"))
}

fn wait_for_parent(parent_pid: u32) -> Result<(), String> {
    // SAFETY: OpenProcess is called with a valid PID and requests synchronization
    // access only. The returned handle is checked before use and always closed.
    let parent = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, parent_pid) };
    if parent.is_null() {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
            // The application can finish shutting down before the helper is
            // scheduled. In that case it is already safe to start the installer.
            return Ok(());
        }
        return Err(format!(
            "Failed to wait for Listen Moe to close (process {parent_pid}): {error}"
        ));
    }

    // SAFETY: parent is a valid process handle opened for synchronization.
    let wait_result = unsafe { WaitForSingleObject(parent, INFINITE) };
    let wait_error = (wait_result == WAIT_FAILED).then(std::io::Error::last_os_error);
    // SAFETY: parent is a valid owned handle and is closed exactly once.
    unsafe {
        CloseHandle(parent);
    }

    match wait_result {
        WAIT_OBJECT_0 => Ok(()),
        WAIT_FAILED => Err(format!(
            "Failed while waiting for Listen Moe to close: {}",
            wait_error.expect("WAIT_FAILED must capture the Windows error")
        )),
        result => Err(format!(
            "Windows returned an unexpected wait result ({result}) while closing Listen Moe."
        )),
    }
}

fn show_update_error(error: &str) {
    let title = wide_null(OsStr::new("Listen Moe update"));
    let message = wide_null(OsStr::new(&format!(
        "Couldn't start the installer.\r\n\r\n{error}"
    )));

    // SAFETY: both UTF-16 buffers are NUL-terminated and remain alive for the call.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

fn wide_null(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
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

#[cfg(test)]
mod tests {
    use super::{parse_install_update_args, InstallUpdateArgs, INSTALL_UPDATE_COMMAND};
    use std::ffi::OsString;
    use std::path::PathBuf;

    fn command_args(parts: &[&str]) -> Vec<OsString> {
        std::iter::once(OsString::from("listenmoe.exe"))
            .chain(parts.iter().map(OsString::from))
            .collect()
    }

    fn assert_invalid(parts: &[&str]) {
        assert!(matches!(
            parse_install_update_args(&command_args(parts)),
            Some(Err(_))
        ));
    }

    #[test]
    fn ignores_unrelated_commands() {
        assert!(parse_install_update_args(&command_args(&["--version"])).is_none());
    }

    #[test]
    fn parses_installer_path_with_spaces() {
        let installer_path = r"C:\Users\Moe Fan\AppData\Local\listenmoe\updates\listenmoe.msi";
        let parsed = parse_install_update_args(&command_args(&[
            INSTALL_UPDATE_COMMAND,
            "42",
            installer_path,
        ]))
        .expect("private update command")
        .expect("valid update arguments");

        assert_eq!(
            parsed,
            InstallUpdateArgs {
                parent_pid: 42,
                installer_path: PathBuf::from(installer_path),
                log_path: PathBuf::from(installer_path).with_extension("install.log"),
            }
        );
    }

    #[test]
    fn accepts_uppercase_msi_extension() {
        let parsed = parse_install_update_args(&command_args(&[
            INSTALL_UPDATE_COMMAND,
            "42",
            r"C:\Updates\listenmoe.MSI",
        ]));

        assert!(matches!(parsed, Some(Ok(_))));
    }

    #[test]
    fn rejects_invalid_process_ids() {
        assert_invalid(&[INSTALL_UPDATE_COMMAND, "0", r"C:\Updates\listenmoe.msi"]);
        assert_invalid(&[
            INSTALL_UPDATE_COMMAND,
            "not-a-pid",
            r"C:\Updates\listenmoe.msi",
        ]);
    }

    #[test]
    fn rejects_missing_or_extra_arguments() {
        assert_invalid(&[INSTALL_UPDATE_COMMAND, "42"]);
        assert_invalid(&[
            INSTALL_UPDATE_COMMAND,
            "42",
            r"C:\Updates\listenmoe.msi",
            "extra",
        ]);
    }

    #[test]
    fn rejects_relative_or_non_msi_paths() {
        assert_invalid(&[INSTALL_UPDATE_COMMAND, "42", "listenmoe.msi"]);
        assert_invalid(&[INSTALL_UPDATE_COMMAND, "42", r"C:\Updates\listenmoe.exe"]);
    }
}

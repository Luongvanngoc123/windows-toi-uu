use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime},
};

use serde::{Deserialize, Serialize};
use winreg::{RegKey, RegValue, enums::*};

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const APPROVED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const TEMP_AGE: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone, Debug)]
pub struct StartupEntry {
    pub name: String,
    pub command: String,
    pub location: String,
    pub enabled: bool,
    pub requires_admin: bool,
    machine: bool,
    approved_key: String,
    value_name: String,
}

#[derive(Clone, Debug, Default)]
pub struct CleanupPreview {
    pub files: u64,
    pub bytes: u64,
}

#[derive(Default)]
pub struct CleanupResult {
    pub files_removed: u64,
    pub bytes_freed: u64,
    pub skipped: u64,
}

pub struct TrimResult {
    pub processes_trimmed: u64,
    pub skipped: u64,
}

pub fn is_admin() -> bool {
    unsafe { windows_sys::Win32::UI::Shell::IsUserAnAdmin() != 0 }
}

fn hive(machine: bool) -> RegKey {
    RegKey::predef(if machine {
        HKEY_LOCAL_MACHINE
    } else {
        HKEY_CURRENT_USER
    })
}

fn startup_enabled(machine: bool, path: &str, name: &str) -> Result<bool, String> {
    let key = match hive(machine).open_subkey(path) {
        Ok(key) => key,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(e) => return Err(format!("Cannot read startup status: {e}")),
    };
    match key.get_raw_value(name) {
        Ok(value) => Ok(!matches!(value.bytes.first(), Some(3 | 7))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(format!("Cannot read startup status for {name}: {e}")),
    }
}

pub fn startup_entries() -> Result<Vec<StartupEntry>, String> {
    let mut entries = Vec::new();
    for machine in [false, true] {
        for (run_path, group, label) in [
            (RUN.to_owned(), "Run", "Run"),
            (
                r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Run".to_owned(),
                "Run32",
                "Run (32-bit)",
            ),
        ] {
            let key = match hive(machine).open_subkey(&run_path) {
                Ok(key) => key,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(format!("Cannot read {run_path}: {e}")),
            };
            let approved_key = format!(r"{APPROVED}\{group}");
            for value in key.enum_values() {
                let (name, raw) = value.map_err(|e| format!("Cannot enumerate startup: {e}"))?;
                if !matches!(raw.vtype, REG_SZ | REG_EXPAND_SZ) {
                    continue;
                }
                let command = decode_registry_string(&raw.bytes);
                if command.is_empty() {
                    continue;
                }
                entries.push(StartupEntry {
                    enabled: startup_enabled(machine, &approved_key, &name)?,
                    name: name.clone(),
                    command,
                    location: format!(
                        "{} / {label}",
                        if machine { "All users" } else { "Current user" }
                    ),
                    requires_admin: machine,
                    machine,
                    approved_key: approved_key.clone(),
                    value_name: name,
                });
            }
        }
    }
    for (machine, root) in [
        (
            false,
            dirs::data_dir().map(|p| p.join(r"Microsoft\Windows\Start Menu\Programs\Startup")),
        ),
        (
            true,
            std::env::var_os("ProgramData")
                .map(PathBuf::from)
                .map(|p| p.join(r"Microsoft\Windows\Start Menu\Programs\Startup")),
        ),
    ] {
        let Some(root) = root else {
            continue;
        };
        let items = match fs::read_dir(&root) {
            Ok(items) => items,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("Cannot read {}: {e}", root.display())),
        };
        let approved_key = format!(r"{APPROVED}\StartupFolder");
        for item in items {
            let item = item.map_err(|e| format!("Cannot enumerate Startup folder: {e}"))?;
            let path = item.path();
            if !item.file_type().map_err(|e| e.to_string())?.is_file() {
                continue;
            }
            let name = item.file_name().to_string_lossy().into_owned();
            if name.eq_ignore_ascii_case("desktop.ini") {
                continue;
            }
            entries.push(StartupEntry {
                enabled: startup_enabled(machine, &approved_key, &name)?,
                name: path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                command: path.display().to_string(),
                location: format!(
                    "{} / Startup folder",
                    if machine { "All users" } else { "Current user" }
                ),
                requires_admin: machine,
                machine,
                approved_key: approved_key.clone(),
                value_name: name,
            });
        }
    }
    entries.sort_by_key(|entry| entry.name.to_lowercase());
    Ok(entries)
}

fn decode_registry_string(bytes: &[u8]) -> String {
    let words: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .take_while(|c| *c != 0)
        .collect();
    String::from_utf16_lossy(&words)
}

#[derive(Serialize, Deserialize)]
struct StartupBackup {
    machine: bool,
    key: String,
    name: String,
    original: Option<Vec<u8>>,
}

fn backup_path() -> Result<PathBuf, String> {
    dirs::data_local_dir()
        .map(|p| p.join("Velo").join("startup-backup.json"))
        .ok_or_else(|| "Cannot locate the user's local app data folder.".into())
}

fn remember_startup(entry: &StartupEntry, original: Option<Vec<u8>>) -> Result<(), String> {
    let path = backup_path()?;
    let mut backups: Vec<StartupBackup> = match fs::read(&path) {
        Ok(data) => {
            serde_json::from_slice(&data).map_err(|e| format!("Startup backup is invalid: {e}"))?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![],
        Err(e) => return Err(format!("Cannot read startup backup: {e}")),
    };
    if backups.iter().any(|b| {
        b.machine == entry.machine && b.key == entry.approved_key && b.name == entry.value_name
    }) {
        return Ok(());
    }
    backups.push(StartupBackup {
        machine: entry.machine,
        key: entry.approved_key.clone(),
        name: entry.value_name.clone(),
        original,
    });
    fs::create_dir_all(path.parent().unwrap())
        .map_err(|e| format!("Cannot create startup backup folder: {e}"))?;
    let data = serde_json::to_vec_pretty(&backups).map_err(|e| e.to_string())?;
    fs::write(&path, data).map_err(|e| format!("Cannot save startup backup: {e}"))
}

pub fn set_startup_enabled(entry: &StartupEntry, enabled: bool) -> Result<(), String> {
    if entry.requires_admin && !is_admin() {
        return Err("Run Velo as Administrator to change an all-users startup item.".into());
    }
    let (key, _) = hive(entry.machine)
        .create_subkey(&entry.approved_key)
        .map_err(|e| format!("Cannot open startup status for writing: {e}"))?;
    let original = match key.get_raw_value(&entry.value_name) {
        Ok(value) if value.vtype == REG_BINARY => Some(value.bytes),
        Ok(_) => {
            return Err(
                "The startup status has an unexpected registry type; no change was made.".into(),
            );
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("Cannot read original startup status: {e}")),
    };
    remember_startup(entry, original)?;
    // StartupApproved is also used by Windows Task Manager; keep the Run command intact.
    let mut bytes = vec![0_u8; 12];
    bytes[0] = if enabled { 2 } else { 3 };
    key.set_raw_value(
        &entry.value_name,
        &RegValue {
            bytes,
            vtype: REG_BINARY,
        },
    )
    .map_err(|e| format!("Cannot update startup status: {e}"))
}

fn user_temp() -> Result<PathBuf, String> {
    let root = dirs::data_local_dir()
        .ok_or("Cannot locate the user's Temp folder.")?
        .join("Temp");
    let metadata =
        fs::symlink_metadata(&root).map_err(|e| format!("Cannot inspect Temp folder: {e}"))?;
    if !metadata.is_dir() || is_reparse(&metadata) {
        return Err("Temp must be a normal local directory, not a link or junction.".into());
    }
    root.canonicalize()
        .map_err(|e| format!("Cannot resolve Temp folder: {e}"))
}

fn is_reparse(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0
}

fn eligible(metadata: &fs::Metadata, now: SystemTime) -> bool {
    metadata.is_file()
        && !is_reparse(metadata)
        && metadata
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age >= TEMP_AGE)
}

fn delete_checked(root: &Path, path: &Path, now: SystemTime) -> std::io::Result<u64> {
    use std::os::windows::{ffi::OsStringExt, fs::OpenOptionsExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_DISPOSITION_INFO, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        FILE_SHARE_READ, FILE_SHARE_WRITE, FileDispositionInfo, GetFinalPathNameByHandleW,
        SetFileInformationByHandle,
    };
    let file = fs::OpenOptions::new()
        .access_mode(DELETE | FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let metadata = file.metadata()?;
    if !eligible(&metadata, now) {
        return Err(std::io::Error::other("File changed since the scan."));
    }
    let mut buffer = vec![0_u16; 32768];
    let length = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            0,
        )
    } as usize;
    if length == 0 || length >= buffer.len() {
        return Err(std::io::Error::other("Cannot verify the final file path."));
    }
    let resolved = PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length]));
    if !resolved.starts_with(root) {
        return Err(std::io::Error::other(
            "File is outside the cleanup directory.",
        ));
    }
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    // Delete the verified handle, so a junction/path swap cannot redirect the deletion.
    let success = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfo,
            (&disposition as *const FILE_DISPOSITION_INFO).cast(),
            std::mem::size_of_val(&disposition) as u32,
        )
    };
    if success == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(metadata.len())
}

fn walk_temp(
    root: &Path,
    path: &Path,
    remove: bool,
    now: SystemTime,
    depth: usize,
    result: &mut CleanupResult,
) {
    if depth > 64 {
        result.skipped += 1;
        return;
    }
    let Ok(items) = fs::read_dir(path) else {
        result.skipped += 1;
        return;
    };
    for item in items {
        let Ok(item) = item else {
            result.skipped += 1;
            continue;
        };
        let path = item.path();
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            result.skipped += 1;
            continue;
        };
        if is_reparse(&metadata) {
            result.skipped += 1;
            continue;
        }
        if metadata.is_dir() {
            walk_temp(root, &path, remove, now, depth + 1, result);
            continue;
        }
        if !eligible(&metadata, now) {
            result.skipped += 1;
            continue;
        }
        let Ok(resolved) = path.canonicalize() else {
            result.skipped += 1;
            continue;
        };
        if !resolved.starts_with(root) {
            result.skipped += 1;
            continue;
        }
        let deleted_bytes = if remove {
            delete_checked(root, &path, now)
        } else {
            Ok(metadata.len())
        };
        if let Ok(bytes) = deleted_bytes {
            result.files_removed += 1;
            result.bytes_freed = result.bytes_freed.saturating_add(bytes);
        } else {
            result.skipped += 1;
        }
    }
}

pub fn cleanup_preview() -> Result<CleanupPreview, String> {
    let root = user_temp()?;
    let mut result = CleanupResult::default();
    walk_temp(&root, &root, false, SystemTime::now(), 0, &mut result);
    Ok(CleanupPreview {
        files: result.files_removed,
        bytes: result.bytes_freed,
    })
}

pub fn cleanup_temp() -> Result<CleanupResult, String> {
    let root = user_temp()?;
    let mut result = CleanupResult::default();
    walk_temp(&root, &root, true, SystemTime::now(), 0, &mut result);
    Ok(result)
}

pub fn trim_memory() -> Result<TrimResult, String> {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::{
            ProcessStatus::EmptyWorkingSet,
            Threading::{OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_SET_QUOTA},
        },
    };
    let sys = sysinfo::System::new_all();
    let own_pid = sysinfo::Pid::from_u32(std::process::id());
    let own_user = sys
        .process(own_pid)
        .and_then(|p| p.user_id())
        .ok_or("Cannot determine the current process owner.")?;
    let mut result = TrimResult {
        processes_trimmed: 0,
        skipped: 0,
    };
    for process in sys.processes().values() {
        if process.user_id() != Some(own_user) || process.pid() == own_pid {
            result.skipped += 1;
            continue;
        }
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_INFORMATION | PROCESS_SET_QUOTA,
                0,
                process.pid().as_u32(),
            )
        };
        if handle.is_null() {
            result.skipped += 1;
            continue;
        }
        let trimmed = unsafe { EmptyWorkingSet(handle) != 0 };
        unsafe {
            CloseHandle(handle);
        }
        if trimmed {
            result.processes_trimmed += 1;
        } else {
            result.skipped += 1;
        }
    }
    Ok(result)
}

fn run_powercfg(args: &[&str]) -> Result<String, String> {
    use std::os::windows::process::CommandExt;
    let path = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .ok_or("SystemRoot is missing.")?
        .join(r"System32\powercfg.exe");
    let output = Command::new(path)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("Cannot run powercfg: {e}"))?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        let detail = if error.trim().is_empty() {
            String::from_utf8_lossy(&output.stdout)
        } else {
            error
        };
        return Err(format!(
            "Windows rejected the power plan change: {}",
            detail.trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

pub fn power_plan() -> Result<String, String> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        System::Power::{PowerGetActiveScheme, PowerReadFriendlyName},
    };
    let mut scheme = std::ptr::null_mut();
    let status = unsafe { PowerGetActiveScheme(std::ptr::null_mut(), &mut scheme) };
    if status != 0 || scheme.is_null() {
        return Err(format!("Cannot read the active power plan ({status})."));
    }
    let mut name = vec![0_u16; 512];
    let mut bytes = (name.len() * 2) as u32;
    let status = unsafe {
        PowerReadFriendlyName(
            std::ptr::null_mut(),
            scheme,
            std::ptr::null(),
            std::ptr::null(),
            name.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    unsafe {
        LocalFree(scheme.cast());
    }
    if status != 0 {
        return Err(format!("Cannot read the power plan name ({status})."));
    }
    let end = name.iter().position(|c| *c == 0).unwrap_or(name.len());
    Ok(String::from_utf16_lossy(&name[..end]))
}

pub fn set_power_plan(high_performance: bool) -> Result<(), String> {
    run_powercfg(&[
        "/setactive",
        if high_performance {
            "8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c"
        } else {
            "381b4222-f694-41f0-9685-ff5bb260df2e"
        },
    ])
    .map(|_| ())
}

fn wide(value: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    value.encode_wide().chain(Some(0)).collect()
}

fn shell_open(target: &std::ffi::OsStr, operation: &str) -> Result<(), String> {
    let target = wide(target);
    let operation = wide(std::ffi::OsStr::new(operation));
    let result = unsafe {
        windows_sys::Win32::UI::Shell::ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
        )
    };
    let code = result as isize;
    if code <= 32 {
        return Err(format!(
            "Windows could not open the requested target (ShellExecute error {code})."
        ));
    }
    Ok(())
}

pub fn open_url(url: &str) -> Result<(), String> {
    if !(url.starts_with("https://")
        || url.starts_with("ms-settings:")
        || url == "windowsdefender:")
        || url.contains('\0')
    {
        return Err("Unsupported URL scheme.".into());
    }
    shell_open(std::ffi::OsStr::new(url), "open")
}

pub fn restart_as_admin() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("Cannot locate Velo executable: {e}"))?;
    shell_open(exe.as_os_str(), "runas")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_strings_stop_at_nul() {
        let bytes: Vec<u8> = "C:\\Tool.exe\0unused"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(decode_registry_string(&bytes), "C:\\Tool.exe");
    }

    #[test]
    fn cleanup_only_removes_old_files_inside_fixture() {
        let root = std::env::temp_dir().join(format!("velo-cleanup-test-{}", std::process::id()));
        fs::create_dir_all(root.join("nested")).unwrap();
        let old = root.join("nested/old.tmp");
        let recent = root.join("recent.tmp");
        fs::write(&old, b"old").unwrap();
        fs::write(&recent, b"recent").unwrap();
        fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(SystemTime::UNIX_EPOCH))
            .unwrap();
        let root = root.canonicalize().unwrap();
        let mut preview = CleanupResult::default();
        walk_temp(&root, &root, false, SystemTime::now(), 0, &mut preview);
        assert_eq!((preview.files_removed, preview.bytes_freed), (1, 3));
        assert!(old.exists());
        let mut cleaned = CleanupResult::default();
        walk_temp(&root, &root, true, SystemTime::now(), 0, &mut cleaned);
        assert_eq!((cleaned.files_removed, cleaned.bytes_freed), (1, 3));
        assert!(!old.exists());
        assert!(recent.exists());
        fs::remove_file(recent).unwrap();
        fs::remove_dir(root.join("nested")).unwrap();
        fs::remove_dir(root).unwrap();
    }
}

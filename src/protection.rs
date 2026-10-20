use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fs, path::PathBuf};

const BEGIN: &str = "# BEGIN VELO MANAGED BLOCKLIST";
const END: &str = "# END VELO MANAGED BLOCKLIST";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BlockProfile {
    pub id: String,
    pub name: String,
    pub domains: Vec<String>,
    pub enabled: bool,
    pub builtin: bool,
}

pub fn default_profiles() -> Vec<BlockProfile> {
    [
        (
            "360",
            "360 Security",
            &[
                "360.cn",
                "www.360.cn",
                "down.360safe.com",
                "dl.360safe.com",
                "360safe.com",
                "www.360safe.com",
            ][..],
        ),
        (
            "wps",
            "WPS Office",
            &[
                "wps.com",
                "www.wps.com",
                "wdl1.pcfg.cache.wpscdn.com",
                "wdl1.cache.wps.cn",
                "pc.wps.cn",
                "platform.wps.cn",
            ][..],
        ),
        ("tencent", "Tencent PC Manager", &["guanjia.qq.com"][..]),
        (
            "uc",
            "UC Browser",
            &["ucweb.com", "www.ucweb.com", "pc.uc.cn"][..],
        ),
    ]
    .into_iter()
    .map(|(id, name, domains)| BlockProfile {
        id: id.into(),
        name: name.into(),
        domains: domains.iter().map(|v| (*v).into()).collect(),
        enabled: false,
        builtin: true,
    })
    .collect()
}

fn data_dir() -> Result<PathBuf, String> {
    dirs::data_local_dir()
        .map(|p| p.join("Velo"))
        .ok_or_else(|| "Cannot find the local application data folder.".into())
}

fn profiles_path() -> Result<PathBuf, String> {
    Ok(data_dir()?.join("protection.json"))
}

pub fn load_profiles() -> Result<Vec<BlockProfile>, String> {
    let path = profiles_path()?;
    if !path.exists() {
        return Ok(default_profiles());
    }
    let contents = fs::read(&path).map_err(|e| format!("Read protection settings: {e}"))?;
    let profiles: Vec<BlockProfile> = serde_json::from_slice(&contents)
        .map_err(|e| format!("Protection settings are invalid: {e}"))?;
    validate_profiles(&profiles)?;
    Ok(profiles)
}

pub fn save_profiles(profiles: &[BlockProfile]) -> Result<(), String> {
    validate_profiles(profiles)?;
    let path = profiles_path()?;
    fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec_pretty(profiles).map_err(|e| e.to_string())?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, bytes).map_err(|e| format!("Write protection settings: {e}"))?;
    fs::rename(temporary, path).map_err(|e| format!("Save protection settings: {e}"))
}

pub fn validate_domain(input: &str) -> Result<String, String> {
    let input = input.trim();
    let domain = input
        .strip_suffix('.')
        .unwrap_or(input)
        .to_ascii_lowercase();
    if domain.len() > 253
        || !domain.contains('.')
        || domain.parse::<std::net::IpAddr>().is_ok()
        || domain.eq_ignore_ascii_case("localhost")
        || domain.ends_with(".localhost")
        || domain.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        })
    {
        return Err(format!(
            "Invalid domain: {input}. Enter a hostname such as download.example.com, without a URL or wildcard."
        ));
    }
    Ok(domain)
}

fn validate_profiles(profiles: &[BlockProfile]) -> Result<(), String> {
    let mut ids = BTreeSet::new();
    for profile in profiles {
        if profile.id.trim().is_empty() || !ids.insert(&profile.id) {
            return Err("Every profile must have a unique identifier.".into());
        }
        if profile.name.trim().is_empty() {
            return Err("Application name cannot be empty.".into());
        }
        for domain in &profile.domains {
            validate_domain(domain)?;
        }
        if profile.enabled && profile.domains.is_empty() {
            return Err(format!("{} has no download domains.", profile.name));
        }
    }
    Ok(())
}

fn blocked_domains(profiles: &[BlockProfile]) -> Result<BTreeSet<String>, String> {
    validate_profiles(profiles)?;
    profiles
        .iter()
        .filter(|profile| profile.enabled)
        .flat_map(|profile| profile.domains.iter().map(|domain| validate_domain(domain)))
        .collect()
}

// Preserve all bytes outside our marker lines, including existing comments and encoding.
fn render_hosts(original: &[u8], domains: &BTreeSet<String>) -> Result<Vec<u8>, String> {
    if original.starts_with(&[0xff, 0xfe])
        || original.starts_with(&[0xfe, 0xff])
        || original.contains(&0)
    {
        return Err("Hosts dùng UTF-16 hoặc định dạng không hỗ trợ. Velo giữ nguyên file; chuyển sang UTF-8 trước khi áp dụng.".into());
    }
    let mut begin = None;
    let mut end = None;
    let mut offset = 0;
    for line in original.split_inclusive(|b| *b == b'\n') {
        let content = line.strip_suffix(b"\n").unwrap_or(line);
        let content = content.strip_suffix(b"\r").unwrap_or(content);
        if content == BEGIN.as_bytes() {
            if begin.is_some() {
                return Err("The hosts file contains duplicate Velo block markers; repair it before continuing.".into());
            }
            begin = Some(offset);
        }
        if content == END.as_bytes() {
            if end.is_some() || begin.is_none() {
                return Err("The hosts file contains invalid Velo block markers; repair it before continuing.".into());
            }
            end = Some(offset + line.len());
        }
        offset += line.len();
    }
    if begin.is_some() != end.is_some() {
        return Err(
            "The hosts file contains an incomplete Velo block. No changes were made.".into(),
        );
    }
    let mut result = match (begin, end) {
        (Some(start), Some(finish)) if finish >= start => {
            let mut bytes = original[..start].to_vec();
            bytes.extend_from_slice(&original[finish..]);
            bytes
        }
        (None, None) => original.to_vec(),
        _ => return Err("The hosts file contains invalid Velo block markers.".into()),
    };
    if !domains.is_empty() {
        if !result.is_empty() && !result.ends_with(b"\n") {
            result.extend_from_slice(b"\r\n");
        }
        result.extend_from_slice(BEGIN.as_bytes());
        result.extend_from_slice(b"\r\n");
        for domain in domains {
            result.extend_from_slice(format!("0.0.0.0 {domain}\r\n:: {domain}\r\n").as_bytes());
        }
        result.extend_from_slice(END.as_bytes());
        result.extend_from_slice(b"\r\n");
    }
    Ok(result)
}

fn hosts_path() -> PathBuf {
    PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()))
        .join(r"System32\drivers\etc\hosts")
}

pub fn protection_status() -> Result<bool, String> {
    let original = fs::read(hosts_path()).map_err(|e| format!("Read hosts file: {e}"))?;
    let without_managed = render_hosts(&original, &BTreeSet::new())?;
    Ok(original != without_managed)
}

pub fn profiles_match_hosts(profiles: &[BlockProfile]) -> Result<bool, String> {
    let original = fs::read(hosts_path()).map_err(|e| format!("Read hosts file: {e}"))?;
    Ok(render_hosts(&original, &blocked_domains(profiles)?)? == original)
}

pub fn apply_profiles(profiles: &[BlockProfile]) -> Result<(), String> {
    let domains = blocked_domains(profiles)?;
    // Save preferences before touching hosts; a save failure must not modify Windows.
    save_profiles(profiles)?;
    update_hosts(&domains)
}

pub fn restore_hosts() -> Result<(), String> {
    let mut profiles = load_profiles()?;
    for profile in &mut profiles {
        profile.enabled = false;
    }
    apply_profiles(&profiles)
}

fn update_hosts(domains: &BTreeSet<String>) -> Result<(), String> {
    let path = hosts_path();
    replace_hosts(&path, &data_dir()?.join("backups"), domains)?;
    // Windows and browsers may keep their own DNS cache; this clears the OS cache only.
    let ipconfig = PathBuf::from(std::env::var_os("SystemRoot").ok_or("SystemRoot is missing.")?)
        .join(r"System32\ipconfig.exe");
    let _ = hidden_command(ipconfig.to_str().ok_or("Invalid Windows path.")?)
        .arg("/flushdns")
        .output();
    Ok(())
}

fn replace_hosts(
    path: &std::path::Path,
    backup_dir: &std::path::Path,
    domains: &BTreeSet<String>,
) -> Result<(), String> {
    let original = fs::read(path).map_err(|e| format!("Read hosts file: {e}"))?;
    let updated = render_hosts(&original, domains)?;
    if original == updated {
        return Ok(());
    }
    fs::create_dir_all(backup_dir).map_err(|e| format!("Create hosts backup folder: {e}"))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let backup = backup_dir.join(format!("hosts-{stamp}.bak"));
    fs::write(&backup, &original).map_err(|e| format!("Back up hosts file: {e}"))?;
    let temporary = path.with_file_name(format!("hosts.velo-{}-{stamp}.tmp", std::process::id()));
    {
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|e| format!("Write hosts file; run Velo as administrator: {e}"))?;
        file.write_all(&updated)
            .and_then(|_| file.sync_all())
            .map_err(|e| format!("Write hosts file: {e}"))?;
    }
    if fs::read(path).map_err(|e| e.to_string())? != original {
        let _ = fs::remove_file(&temporary);
        return Err("Hosts vừa bị thay đổi bởi chương trình khác. Hãy thử lại.".into());
    }
    if let Err(error) = replace_existing_file(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(format!(
            "Replace hosts file; run Velo as administrator: {error}"
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn replace_existing_file(
    replacement: &std::path::Path,
    target: &std::path::Path,
) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;
    let target: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    let replacement: Vec<u16> = replacement
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    // ReplaceFile preserves the existing hosts file's ACL; rename would inherit the temp file's ACL.
    let success = unsafe {
        ReplaceFileW(
            target.as_ptr(),
            replacement.as_ptr(),
            std::ptr::null(),
            0,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if success == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace_existing_file(
    replacement: &std::path::Path,
    target: &std::path::Path,
) -> std::io::Result<()> {
    fs::rename(replacement, target)
}

#[cfg(windows)]
fn hidden_command(program: &str) -> std::process::Command {
    use std::os::windows::process::CommandExt;
    let mut command = std::process::Command::new(program);
    command.creation_flags(0x08000000);
    command
}

#[cfg(not(windows))]
fn hidden_command(program: &str) -> std::process::Command {
    std::process::Command::new(program)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_and_normalizes_hostnames() {
        assert_eq!(validate_domain("Example.COM.").unwrap(), "example.com");
        for value in [
            "https://example.com/a",
            "*.example.com",
            "localhost",
            "127.0.0.1",
            "x..com",
            "-x.com",
            "example.com\n0.0.0.0 other.com",
        ] {
            assert!(validate_domain(value).is_err(), "{value}");
        }
    }

    #[test]
    fn hosts_roundtrip_preserves_unmanaged_bytes() {
        let original = b"# existing \xff\r\n127.0.0.1 localhost\r\n";
        let domains = BTreeSet::from(["example.com".into()]);
        let blocked = render_hosts(original, &domains).unwrap();
        assert!(blocked.starts_with(original));
        assert_eq!(render_hosts(&blocked, &domains).unwrap(), blocked);
        assert_eq!(render_hosts(&blocked, &BTreeSet::new()).unwrap(), original);
    }

    #[test]
    fn rejects_broken_managed_hosts_blocks() {
        assert!(render_hosts(&[0xff, 0xfe, b'#', 0], &BTreeSet::new()).is_err());
        assert!(render_hosts(format!("{BEGIN}\nvalue\n").as_bytes(), &BTreeSet::new()).is_err());
        assert!(render_hosts(format!("{END}\n{BEGIN}\n").as_bytes(), &BTreeSet::new()).is_err());
        assert!(
            render_hosts(
                format!("{BEGIN}\n{END}\n{BEGIN}\n{END}\n").as_bytes(),
                &BTreeSet::new()
            )
            .is_err()
        );
    }

    #[test]
    fn presets_validate_without_enabling_anything() {
        let defaults = default_profiles();
        validate_profiles(&defaults).unwrap();
        assert!(blocked_domains(&defaults).unwrap().is_empty());
    }

    #[test]
    fn file_apply_and_restore_keep_a_backup() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("velo-hosts-test-{}-{stamp}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("hosts");
        let backup_dir = directory.join("backups");
        let original = b"127.0.0.1 localhost\r\n# untouched\r\n";
        fs::write(&path, original).unwrap();
        replace_hosts(&path, &backup_dir, &BTreeSet::from(["example.com".into()])).unwrap();
        let blocked = fs::read(&path).unwrap();
        assert!(blocked.starts_with(original));
        let backups: Vec<_> = fs::read_dir(&backup_dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read(&backups[0]).unwrap(), original);
        replace_hosts(&path, &backup_dir, &BTreeSet::new()).unwrap();
        assert_eq!(fs::read(&path).unwrap(), original);
        fs::remove_dir_all(&directory).unwrap();
    }
}

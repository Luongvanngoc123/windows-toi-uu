use std::{fs, path::PathBuf, process::Command};
use winreg::{RegKey, enums::*};

#[derive(Clone, Debug)]
pub struct RuntimePackage {
    pub id: String,
    pub name: String,
    pub installed: bool,
    pub detail: String,
}

const PACKAGES: &[(&str, &str)] = &[
    ("Microsoft.VCRedist.2015+.x64", "Visual C++ 2015+ · x64"),
    ("Microsoft.VCRedist.2015+.x86", "Visual C++ 2015+ · x86"),
    (
        "Microsoft.DotNet.DesktopRuntime.8",
        ".NET Desktop Runtime 8 · x64",
    ),
    (
        "Microsoft.DotNet.DesktopRuntime.10",
        ".NET Desktop Runtime 10 · x64",
    ),
    (
        "Microsoft.EdgeWebView2Runtime",
        "Microsoft Edge WebView2 Runtime",
    ),
];

fn hidden_winget() -> Command {
    use std::os::windows::process::CommandExt;
    let alias = dirs::data_local_dir()
        .unwrap_or_default()
        .join(r"Microsoft\WindowsApps\winget.exe");
    let mut command = Command::new(alias);
    command.creation_flags(0x08000000);
    command
}

pub fn winget_available() -> bool {
    hidden_winget()
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn vc_version(architecture: &str) -> Option<String> {
    for view in [KEY_WOW64_64KEY, KEY_WOW64_32KEY] {
        let path = format!(r"SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\{architecture}");
        if let Ok(key) =
            RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey_with_flags(path, KEY_READ | view)
            && key.get_value::<u32, _>("Installed").unwrap_or(0) == 1
        {
            return key
                .get_value::<String, _>("Version")
                .ok()
                .or_else(|| Some("14.x".into()));
        }
    }
    None
}

fn stable_version(version: &str, major: u32) -> bool {
    let numbers: Vec<_> = version.split('.').map(str::parse::<u32>).collect();
    numbers.len() == 3 && numbers.iter().all(Result::is_ok) && numbers[0] == Ok(major)
}

fn dotnet_version(major: u32) -> Option<String> {
    let root = std::env::var_os("ProgramW6432").or_else(|| std::env::var_os("ProgramFiles"))?;
    let shared = PathBuf::from(root).join(r"dotnet\shared\Microsoft.WindowsDesktop.App");
    let mut versions: Vec<_> = fs::read_dir(shared)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|version| stable_version(version, major))
        .collect();
    versions.sort_by_cached_key(|v| {
        v.split('.')
            .filter_map(|n| n.parse::<u32>().ok())
            .collect::<Vec<_>>()
    });
    versions.pop()
}

fn webview_version() -> Option<String> {
    for hive in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
        for view in [KEY_WOW64_64KEY, KEY_WOW64_32KEY] {
            let Ok(key) = RegKey::predef(hive)
                .open_subkey_with_flags(r"SOFTWARE\Microsoft\EdgeUpdate\Clients", KEY_READ | view)
            else {
                continue;
            };
            for client in key.enum_keys().filter_map(Result::ok) {
                let Ok(client) = key.open_subkey(client) else {
                    continue;
                };
                let name = client.get_value::<String, _>("name").unwrap_or_default();
                if name.to_lowercase().contains("webview2") {
                    let version = client.get_value::<String, _>("pv").unwrap_or_default();
                    if !version.is_empty() && version != "0.0.0.0" {
                        return Some(version);
                    }
                }
            }
        }
    }
    None
}

pub fn scan() -> Result<Vec<RuntimePackage>, String> {
    let versions = [
        vc_version("x64"),
        vc_version("x86"),
        dotnet_version(8),
        dotnet_version(10),
        webview_version(),
    ];
    Ok(PACKAGES
        .iter()
        .zip(versions)
        .map(|((id, name), version)| RuntimePackage {
            id: (*id).into(),
            name: (*name).into(),
            installed: version.is_some(),
            detail: version
                .map(|v| format!("Phiên bản {v}"))
                .unwrap_or_else(|| "Không tìm thấy bản cài tiêu chuẩn".into()),
        })
        .collect())
}

pub fn install_missing(packages: Vec<RuntimePackage>) -> Result<String, String> {
    if packages
        .iter()
        .any(|p| !PACKAGES.iter().any(|(id, _)| *id == p.id))
    {
        return Err("Gói cài không nằm trong danh sách runtime được hỗ trợ.".into());
    }
    if !winget_available() {
        return Err("Không tìm thấy WinGet. Cài App Installer của Microsoft rồi thử lại.".into());
    }
    // Recheck the machine; never rely on a stale UI snapshot to decide what to install.
    let current = scan()?;
    let selected: std::collections::HashSet<_> = packages.iter().map(|p| p.id.as_str()).collect();
    let missing: Vec<_> = current
        .into_iter()
        .filter(|p| selected.contains(p.id.as_str()) && !p.installed)
        .collect();
    if missing.is_empty() {
        return Ok("Các thư viện đã chọn đều đã có trên máy.".into());
    }
    let mut installed = 0;
    let mut errors = Vec::new();
    for package in &missing {
        let output = hidden_winget()
            .args([
                "install",
                "--id",
                &package.id,
                "--exact",
                "--source",
                "winget",
                "--silent",
                "--accept-source-agreements",
                "--accept-package-agreements",
                "--disable-interactivity",
            ])
            .output();
        let detected = scan()?
            .into_iter()
            .any(|p| p.id == package.id && p.installed);
        if detected {
            installed += 1;
            continue;
        }
        let detail = match output {
            Ok(o) => {
                let raw = format!(
                    "{} {}",
                    String::from_utf8_lossy(&o.stdout),
                    String::from_utf8_lossy(&o.stderr)
                );
                let tail: String = raw
                    .chars()
                    .rev()
                    .take(400)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect();
                format!(
                    "{} (mã {}): {}",
                    package.name,
                    o.status.code().unwrap_or(-1),
                    tail.trim()
                )
            }
            Err(e) => format!("{}: {e}", package.name),
        };
        errors.push(detail);
    }
    if errors.is_empty() {
        Ok(format!(
            "Đã cài và kiểm tra {installed} thư viện. Khởi động lại máy nếu bộ cài yêu cầu."
        ))
    } else {
        Err(format!(
            "Đã xác nhận {installed}/{} thư viện. Các mục còn lại chưa được xác nhận; có thể cần khởi động lại hoặc thử lại.\n{}",
            missing.len(),
            errors.join("\n")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn runtime_detection_requires_stable_exact_major() {
        assert!(stable_version("8.0.24", 8));
        for version in ["9.0.1", "8.0.1-preview.1", "8", "8.0", "8.0.foo", "8.0.1.2"] {
            assert!(!stable_version(version, 8));
        }
    }
    #[test]
    fn installer_rejects_unapproved_package_before_launching_any_process() {
        let p = RuntimePackage {
            id: "Other.Untrusted".into(),
            name: "Other".into(),
            installed: false,
            detail: String::new(),
        };
        assert!(install_missing(vec![p]).unwrap_err().contains("không nằm"));
    }
}

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadinessInfo {
    pub kernel_version: String,
    pub kernel_cmdline: String,
    pub kernel_cmdline_params: Vec<String>,
    pub loaded_modules: Vec<String>,
    pub kernel_headers: KernelHeadersInfo,
    pub secure_boot: bool,
    pub ovmf: OvmfInfo,
    pub user_access: UserAccessInfo,
    pub libvirt_domains: Vec<LibvirtDomainInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KernelHeadersInfo {
    pub present: bool,
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OvmfInfo {
    pub code_paths: Vec<PathBuf>,
    pub vars_paths: Vec<PathBuf>,
}

impl OvmfInfo {
    pub fn available(&self) -> bool {
        !self.code_paths.is_empty() && !self.vars_paths.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserAccessInfo {
    pub username: Option<String>,
    pub groups: Vec<String>,
    pub in_libvirt_group: bool,
    pub in_kvm_group: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibvirtDomainInfo {
    pub id: Option<String>,
    pub name: String,
    pub state: String,
}

pub async fn detect() -> Result<ReadinessInfo> {
    detect_from_root(Path::new("/")).await
}

pub async fn detect_from_root(root: impl AsRef<Path>) -> Result<ReadinessInfo> {
    let root = root.as_ref();
    let is_live = is_live_root(root);

    let kernel_version = read_kernel_version(root, is_live).await;
    let kernel_cmdline = tokio::fs::read_to_string(rooted(root, "/proc/cmdline"))
        .await
        .unwrap_or_default()
        .trim()
        .to_string();
    let kernel_cmdline_params = parse_kernel_cmdline(&kernel_cmdline);

    let loaded_modules = tokio::fs::read_to_string(rooted(root, "/proc/modules"))
        .await
        .map(|content| parse_loaded_modules(&content))
        .unwrap_or_default();

    let kernel_headers = detect_kernel_headers(root, &kernel_version);
    let secure_boot = detect_secure_boot_from_root(root).await;
    let ovmf = detect_ovmf(root);
    let user_access = detect_user_access(root, is_live).await;
    let libvirt_domains = detect_libvirt_domains(root, is_live).await;

    Ok(ReadinessInfo {
        kernel_version,
        kernel_cmdline,
        kernel_cmdline_params,
        loaded_modules,
        kernel_headers,
        secure_boot,
        ovmf,
        user_access,
        libvirt_domains,
    })
}

pub fn parse_kernel_cmdline(cmdline: &str) -> Vec<String> {
    cmdline.split_whitespace().map(str::to_string).collect()
}

pub fn parse_loaded_modules(modules: &str) -> Vec<String> {
    let mut parsed: Vec<String> = modules
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_string)
        .collect();
    parsed.sort();
    parsed.dedup();
    parsed
}

pub fn parse_secure_boot_state(bytes: &[u8]) -> bool {
    if let Ok(text) = std::str::from_utf8(bytes) {
        match text.trim() {
            "1" => return true,
            "0" => return false,
            _ => {}
        }
    }

    match bytes {
        [1] | [b'1'] => true,
        [0] | [b'0'] => false,
        bytes if bytes.len() >= 5 => bytes[4] == 1,
        _ => false,
    }
}

pub fn parse_user_access(username: Option<String>, groups: &str) -> UserAccessInfo {
    let mut groups: Vec<String> = groups.split_whitespace().map(str::to_string).collect();
    groups.sort();
    groups.dedup();

    UserAccessInfo {
        username,
        in_libvirt_group: groups.iter().any(|group| group == "libvirt"),
        in_kvm_group: groups.iter().any(|group| group == "kvm"),
        groups,
    }
}

pub fn parse_virsh_list_all(output: &str) -> Vec<LibvirtDomainInfo> {
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }

            let parts: Vec<&str> = line.split_whitespace().collect();
            // Skip the column header (`Id   Name   State`) by matching
            // the literal first two tokens together rather than just
            // `parts[0] == "Id"`. This keeps a domain literally named
            // `Id` from being silently dropped (virsh disallows that
            // name in practice, but defensive parsing is cheap).
            if parts.first() == Some(&"Id") && parts.get(1) == Some(&"Name") {
                return None;
            }
            // Skip the `--------` separator line.
            if line.starts_with("---") {
                return None;
            }
            if parts.len() < 3 {
                return None;
            }

            let id = (parts[0] != "-").then(|| parts[0].to_string());
            let name = parts[1].to_string();
            let state = parts[2..].join(" ");

            Some(LibvirtDomainInfo { id, name, state })
        })
        .collect()
}

async fn read_kernel_version(root: &Path, is_live: bool) -> String {
    let osrelease = rooted(root, "/proc/sys/kernel/osrelease");
    if let Ok(version) = tokio::fs::read_to_string(osrelease).await {
        let version = version.trim();
        if !version.is_empty() {
            return version.to_string();
        }
    }

    if is_live {
        if let Ok(output) = tokio::process::Command::new("uname")
            .arg("-r")
            .output()
            .await
        {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !version.is_empty() {
                return version;
            }
        }
    }

    String::new()
}

fn detect_kernel_headers(root: &Path, kernel_version: &str) -> KernelHeadersInfo {
    let candidates = kernel_header_candidates(kernel_version);
    let path = candidates
        .iter()
        .map(|path| rooted(root, path))
        .find(|path| path.exists());

    KernelHeadersInfo {
        present: path.is_some(),
        path,
    }
}

fn kernel_header_candidates(kernel_version: &str) -> Vec<String> {
    if kernel_version.is_empty() {
        return Vec::new();
    }

    vec![
        format!("/usr/lib/modules/{kernel_version}/build"),
        format!("/lib/modules/{kernel_version}/build"),
        format!("/usr/src/linux-headers-{kernel_version}"),
        format!("/usr/src/kernels/{kernel_version}"),
    ]
}

async fn detect_secure_boot_from_root(root: &Path) -> bool {
    let path = rooted(
        root,
        "/sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c",
    );
    tokio::fs::read(path)
        .await
        .map(|bytes| parse_secure_boot_state(&bytes))
        .unwrap_or(false)
}

fn detect_ovmf(root: &Path) -> OvmfInfo {
    let code_paths = OVMF_CODE_CANDIDATES
        .iter()
        .map(|path| rooted(root, path))
        .filter(|path| path.exists())
        .collect();

    let vars_paths = OVMF_VARS_CANDIDATES
        .iter()
        .map(|path| rooted(root, path))
        .filter(|path| path.exists())
        .collect();

    OvmfInfo {
        code_paths,
        vars_paths,
    }
}

async fn detect_user_access(root: &Path, is_live: bool) -> UserAccessInfo {
    let fixture_groups_path = rooted(root, "/etc/virtu-user-groups");
    if let Ok(content) = tokio::fs::read_to_string(fixture_groups_path).await {
        let mut lines = content.lines();
        let username = lines
            .next()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string);
        let groups = lines.next().unwrap_or_default();
        return parse_user_access(username, groups);
    }

    if is_live {
        let username = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .ok();
        if let Ok(output) = tokio::process::Command::new("id").arg("-nG").output().await {
            let groups = String::from_utf8_lossy(&output.stdout);
            return parse_user_access(username, &groups);
        }
    }

    parse_user_access(None, "")
}

async fn detect_libvirt_domains(root: &Path, is_live: bool) -> Vec<LibvirtDomainInfo> {
    let fixture_output = rooted(root, "/var/lib/libvirt/virsh-list-all");
    if let Ok(content) = tokio::fs::read_to_string(fixture_output).await {
        return parse_virsh_list_all(&content);
    }

    if is_live && which::which("virsh").is_ok() {
        if let Ok(output) = tokio::process::Command::new("virsh")
            .args(["list", "--all"])
            .output()
            .await
        {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                return parse_virsh_list_all(&stdout);
            }
        }
    }

    Vec::new()
}

fn rooted(root: &Path, absolute_path: &str) -> PathBuf {
    root.join(absolute_path.trim_start_matches('/'))
}

fn is_live_root(root: &Path) -> bool {
    root == Path::new("/")
}

const OVMF_CODE_CANDIDATES: &[&str] = &[
    "/usr/share/OVMF/OVMF_CODE.fd",
    "/usr/share/OVMF/OVMF_CODE_4M.fd",
    "/usr/share/OVMF/OVMF_CODE.secboot.fd",
    "/usr/share/edk2/x64/OVMF_CODE.fd",
    "/usr/share/edk2/x64/OVMF_CODE.4m.fd",
    "/usr/share/edk2-ovmf/x64/OVMF_CODE.fd",
    "/usr/share/edk2-ovmf/x64/OVMF_CODE.4m.fd",
    "/usr/share/edk2/ovmf/OVMF_CODE.fd",
    "/usr/share/qemu/ovmf-x86_64-code.bin",
    "/usr/share/qemu/ovmf-x86_64-ms-code.bin",
];

const OVMF_VARS_CANDIDATES: &[&str] = &[
    "/usr/share/OVMF/OVMF_VARS.fd",
    "/usr/share/OVMF/OVMF_VARS_4M.fd",
    "/usr/share/OVMF/OVMF_VARS.secboot.fd",
    "/usr/share/edk2/x64/OVMF_VARS.fd",
    "/usr/share/edk2/x64/OVMF_VARS.4m.fd",
    "/usr/share/edk2-ovmf/x64/OVMF_VARS.fd",
    "/usr/share/edk2-ovmf/x64/OVMF_VARS.4m.fd",
    "/usr/share/edk2/ovmf/OVMF_VARS.fd",
    "/usr/share/qemu/ovmf-x86_64-vars.bin",
    "/usr/share/qemu/ovmf-x86_64-ms-vars.bin",
];

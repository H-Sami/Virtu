//! Detection summary screen (Milestone 10, slice 10.2).
//!
//! Shows the user a read-only snapshot of the current host: CPU,
//! memory, GPUs, IOMMU, distro, bootloader, initramfs, display
//! manager, audio, and the live `CompatibilityReport`.
//!
//! No interactivity besides the global key bindings handled in
//! `tui::mod` (`q` to quit, `Enter` to advance once 10.3 lands the
//! choice flow). The screen is fully renderable from a `SystemProfile`
//! plus a `CompatibilityReport` — both pure-data — so unit tests can
//! pin the rendered output without spinning a terminal.

use crate::detect::SystemProfile;
use crate::engine::{CompatibilityReport, CompatibilityStatus, FindingSeverity};

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

/// Pure-data view of the detection screen. Kept separate from the
/// rendering code so unit tests can pin the visible text without
/// instantiating a terminal.
#[derive(Debug, Clone, PartialEq)]
pub struct DetectionView {
    pub host_lines: Vec<String>,
    pub gpu_lines: Vec<String>,
    pub finding_lines: Vec<FindingLine>,
    pub status: CompatibilityStatus,
}

/// One row of the compatibility report, pre-formatted with its
/// severity-derived color tag.
#[derive(Debug, Clone, PartialEq)]
pub struct FindingLine {
    pub severity: FindingSeverity,
    pub text: String,
}

impl DetectionView {
    /// Build the view from a freshly captured `SystemProfile` and
    /// the matching `CompatibilityReport`. Pure: no host I/O, no
    /// allocation beyond the strings.
    pub fn new(profile: &SystemProfile, report: &CompatibilityReport) -> Self {
        let host_lines = vec![
            format!(
                "Distro       {} ({})",
                profile.distro.pretty_name, profile.distro.id
            ),
            format!("Kernel       {}", profile.readiness.kernel_version),
            format!(
                "CPU          {} ({} cores / {} threads)",
                profile.cpu.model_name.trim(),
                profile.cpu.physical_cores,
                profile.cpu.logical_cores
            ),
            format!(
                "RAM          {:.1} GiB total, {:.1} GiB available",
                profile.ram.total_kb as f64 / 1024.0 / 1024.0,
                profile.ram.available_kb as f64 / 1024.0 / 1024.0
            ),
            format!("Bootloader   {}", profile.bootloader.kind),
            format!("Initramfs    {}", profile.initramfs_system.name()),
            format!("Display Mgr  {}", profile.display_manager),
            format!("Display Srv  {}", profile.display_server),
            format!("Audio        {}", profile.audio),
            format!(
                "IOMMU        {}",
                if profile.iommu_active() {
                    format!("active ({} groups)", profile.iommu_groups.len())
                } else {
                    "NOT active".to_string()
                }
            ),
            format!(
                "Secure Boot  {}",
                if profile.secure_boot {
                    "enabled (signing required for new kernel modules)"
                } else {
                    "disabled"
                }
            ),
        ];

        let gpu_lines = if profile.gpus.is_empty() {
            vec!["(no GPUs detected)".to_string()]
        } else {
            profile
                .gpus
                .iter()
                .map(|gpu| {
                    format!(
                        "{}  {}  {}:{}  driver={}  iommu_group={}  isolated={}",
                        gpu.pci_slot,
                        gpu.model_name,
                        gpu.vendor_id,
                        gpu.device_id,
                        gpu.current_driver.as_deref().unwrap_or("none"),
                        gpu.iommu_group_id
                            .map(|id| id.to_string())
                            .unwrap_or_else(|| "?".to_string()),
                        gpu.iommu_isolated
                    )
                })
                .collect()
        };

        let finding_lines = report
            .findings
            .iter()
            .map(|finding| FindingLine {
                severity: finding.severity,
                text: finding.explanation.clone(),
            })
            .collect();

        DetectionView {
            host_lines,
            gpu_lines,
            finding_lines,
            status: report.status,
        }
    }
}

/// Render the detection screen into the supplied `Frame` area.
pub fn render(frame: &mut Frame, area: Rect, view: &DetectionView) {
    // Top-level vertical split:
    //   [3] header banner
    //   [N] body (two-column host info + GPU list, then findings)
    //   [3] footer with key hints
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(3),
        ])
        .split(area);

    render_header(frame, outer[0], view.status);
    render_body(frame, outer[1], view);
    render_footer(frame, outer[2]);
}

fn render_header(frame: &mut Frame, area: Rect, status: CompatibilityStatus) {
    let (label, color) = match status {
        CompatibilityStatus::Ready => ("Compatibility: READY", Color::Green),
        CompatibilityStatus::Warnings => ("Compatibility: WARNINGS", Color::Yellow),
        CompatibilityStatus::Blocked => ("Compatibility: BLOCKED", Color::Red),
    };
    let title_line = Line::from(vec![
        Span::styled(
            "Virtu — GPU Passthrough Wizard",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("    "),
        Span::styled(
            label,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
    ]);
    let header = Paragraph::new(title_line)
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(header, area);
}

fn render_body(frame: &mut Frame, area: Rect, view: &DetectionView) {
    // Body is split horizontally into host facts (left) and GPU
    // listing (right). Findings render below both as a full-width
    // paragraph.
    let body = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(area);

    let top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(body[0]);

    render_host_block(frame, top[0], view);
    render_gpu_block(frame, top[1], view);
    render_findings_block(frame, body[1], view);
}

fn render_host_block(frame: &mut Frame, area: Rect, view: &DetectionView) {
    let lines: Vec<Line> = view
        .host_lines
        .iter()
        .map(|s| Line::from(s.as_str()))
        .collect();
    let block = Block::default().borders(Borders::ALL).title(" Host ");
    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_gpu_block(frame: &mut Frame, area: Rect, view: &DetectionView) {
    let lines: Vec<Line> = view
        .gpu_lines
        .iter()
        .map(|s| Line::from(s.as_str()))
        .collect();
    let block = Block::default().borders(Borders::ALL).title(" GPUs ");
    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_findings_block(frame: &mut Frame, area: Rect, view: &DetectionView) {
    let lines: Vec<Line> = if view.finding_lines.is_empty() {
        vec![Line::from(Span::styled(
            "No findings — host looks clean.",
            Style::default().fg(Color::Green),
        ))]
    } else {
        view.finding_lines
            .iter()
            .map(|finding| {
                let (prefix, color) = match finding.severity {
                    FindingSeverity::Pass => ("[OK]   ", Color::Green),
                    FindingSeverity::Warn => ("[WARN] ", Color::Yellow),
                    FindingSeverity::Fail => ("[FAIL] ", Color::Red),
                };
                Line::from(vec![
                    Span::styled(
                        prefix,
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(finding.text.clone()),
                ])
            })
            .collect()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Compatibility findings ");
    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_footer(frame: &mut Frame, area: Rect) {
    // Slice 10.2 only ships the detection screen so Enter is a no-op.
    // Slice 10.3 will replace this with a real "Enter: continue".
    let hint = Line::from(vec![
        Span::styled(
            "q",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" quit    "),
        Span::styled(
            "Enter",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" continue (choice flow lands in slice 10.3)"),
    ]);
    let footer = Paragraph::new(hint)
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(footer, area);
}

#[cfg(test)]
pub(crate) mod tests_helpers {
    use crate::detect::audio::AudioSystem;
    use crate::detect::bootloader::{BootloaderInfo, BootloaderKind};
    use crate::detect::cpu::CpuInfo;
    use crate::detect::display_manager::DisplayManager;
    use crate::detect::display_server::DisplayServer;
    use crate::detect::distro::{DistroFamily, DistroInfo, PackageManager};
    use crate::detect::gpu::{GpuInfo, GpuType, GpuVendor};
    use crate::detect::initramfs::InitramfsSystem;
    use crate::detect::memory::MemInfo;
    use crate::detect::readiness::{KernelHeadersInfo, OvmfInfo, ReadinessInfo, UserAccessInfo};
    use crate::detect::storage::StorageInfo;
    use crate::detect::virtualization::VirtInfo;
    use crate::detect::SystemProfile;
    use std::collections::HashMap;
    use std::path::PathBuf;

    /// Build a single-NVIDIA-GPU `SystemProfile` for tests that don't
    /// care about the GPU layout.
    pub fn dummy_profile() -> SystemProfile {
        dummy_profile_with_extras(vec![(
            "0000:01:00.0",
            GpuType::Discrete,
            GpuVendor::Nvidia,
            "10de",
            "1f08",
        )])
    }

    /// Like [`dummy_profile`] but with an explicit list of GPU
    /// descriptors so tests can craft iGPU + dGPU and dual-discrete
    /// shapes without copying every other field.
    pub fn dummy_profile_with_extras(
        gpus: Vec<(&str, GpuType, GpuVendor, &str, &str)>,
    ) -> SystemProfile {
        let gpu_infos = gpus
            .into_iter()
            .map(|(slot, ty, vendor, vendor_id, device_id)| GpuInfo {
                pci_slot: slot.to_string(),
                vendor,
                gpu_type: ty,
                model_name: format!("Test {vendor_id}:{device_id}"),
                vendor_id: vendor_id.to_string(),
                device_id: device_id.to_string(),
                subsystem_vendor_id: "0000".to_string(),
                subsystem_device_id: "0000".to_string(),
                current_driver: None,
                iommu_group_id: Some(1),
                iommu_isolated: true,
                rom_accessible: false,
                companion_audio: None,
                is_boot_vga: false,
                vfio_compatible: true,
                quirks: Vec::new(),
            })
            .collect();

        SystemProfile {
            cpu: CpuInfo {
                vendor: "GenuineIntel".to_string(),
                model_name: "Intel(R) Test CPU".to_string(),
                physical_cores: 8,
                logical_cores: 16,
                numa_nodes: Vec::new(),
                iommu_capable: true,
                iommu_enabled: true,
                has_hyperthreading: true,
                core_to_threads: HashMap::new(),
            },
            gpus: gpu_infos,
            iommu_groups: vec![crate::detect::iommu::IommuGroup {
                id: 17,
                devices: vec![],
                is_isolated_for_gpu: true,
            }],
            ram: MemInfo {
                total_kb: 32 * 1024 * 1024,
                available_kb: 24 * 1024 * 1024,
                hugepages_total: 0,
                hugepages_free: 0,
                hugepage_size_kb: 2048,
            },
            distro: DistroInfo {
                id: "arch".to_string(),
                id_like: Vec::new(),
                pretty_name: "Arch Linux".to_string(),
                version_id: String::new(),
                family: DistroFamily::Arch,
                package_manager: PackageManager::Pacman,
            },
            bootloader: BootloaderInfo {
                kind: BootloaderKind::Grub2,
                config_path: None,
                entry_paths: Vec::new(),
                active_entry: None,
                update_command: None,
                is_uefi: true,
            },
            initramfs_system: InitramfsSystem::Mkinitcpio,
            display_manager: DisplayManager::Sddm,
            display_server: DisplayServer::Wayland,
            audio: AudioSystem::PipeWire,
            monitors: Vec::new(),
            usb_devices: Vec::new(),
            storage: StorageInfo {
                default_vm_dir: PathBuf::from("/var/lib/libvirt/images"),
                available_bytes: 100 * 1024 * 1024 * 1024,
            },
            virtualization: VirtInfo {
                qemu_version: Some("QEMU 8.2".to_string()),
                libvirt_version: Some("10.0".to_string()),
                virsh_available: true,
                virt_manager_available: true,
                libvirtd_running: true,
            },
            readiness: ReadinessInfo {
                kernel_version: "6.10.0-arch1-1".to_string(),
                kernel_cmdline: "BOOT_IMAGE=/vmlinuz".to_string(),
                kernel_cmdline_params: Vec::new(),
                loaded_modules: Vec::new(),
                kernel_headers: KernelHeadersInfo {
                    present: true,
                    path: None,
                },
                secure_boot: false,
                ovmf: OvmfInfo {
                    code_paths: Vec::new(),
                    vars_paths: Vec::new(),
                },
                user_access: UserAccessInfo {
                    username: Some("user".to_string()),
                    groups: vec!["libvirt".to_string(), "kvm".to_string()],
                    in_libvirt_group: true,
                    in_kvm_group: true,
                },
                libvirt_domains: Vec::new(),
            },
            secure_boot: false,
            kernel_cmdline: "BOOT_IMAGE=/vmlinuz".to_string(),
            scan_timestamp: chrono::Utc::now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{
        build_compatibility_report, CompatibilityFinding, CompatibilityReport, FixOption,
    };

    fn empty_report(status: CompatibilityStatus) -> CompatibilityReport {
        CompatibilityReport {
            status,
            findings: Vec::new(),
        }
    }

    fn warn_report() -> CompatibilityReport {
        CompatibilityReport {
            status: CompatibilityStatus::Warnings,
            findings: vec![
                CompatibilityFinding {
                    id: "secure-boot-enabled".to_string(),
                    severity: FindingSeverity::Warn,
                    title: "Secure Boot is on".to_string(),
                    explanation: "Secure Boot is enabled; signing required for new kernel modules."
                        .to_string(),
                    evidence: vec![],
                    fix_options: vec![FixOption::new(
                        "Disable Secure Boot",
                        "Boot into firmware setup and disable Secure Boot before re-running.",
                        crate::engine::FixAutomation::Manual,
                        false,
                    )],
                },
                CompatibilityFinding {
                    id: "iommu-active".to_string(),
                    severity: FindingSeverity::Pass,
                    title: "IOMMU active".to_string(),
                    explanation: "IOMMU groups detected.".to_string(),
                    evidence: vec![],
                    fix_options: vec![],
                },
            ],
        }
    }

    fn dummy_profile() -> SystemProfile {
        // Delegate to the shared helper so the choice-screen tests
        // can build the same fixture profile without duplicating the
        // long initializer.
        tests_helpers::dummy_profile()
    }

    #[test]
    fn detection_view_carries_every_host_fact_and_gpu_summary() {
        let profile = dummy_profile();
        let report = empty_report(CompatibilityStatus::Ready);
        let view = DetectionView::new(&profile, &report);

        // Host block mentions the headline detection facts so the
        // user can confirm at a glance the wizard is looking at the
        // right machine.
        let host_blob = view.host_lines.join("\n");
        assert!(host_blob.contains("Arch Linux"));
        assert!(host_blob.contains("6.10.0-arch1-1"));
        assert!(host_blob.contains("Intel(R) Test CPU"));
        assert!(host_blob.contains("GRUB2"));
        assert!(host_blob.contains("SDDM"));
        assert!(host_blob.contains("Wayland"));
        assert!(host_blob.contains("active (1 groups)"));

        // GPU block lists each detected card with PCI ids + driver.
        assert_eq!(view.gpu_lines.len(), 1);
        let gpu_line = &view.gpu_lines[0];
        assert!(gpu_line.contains("0000:01:00.0"));
        assert!(gpu_line.contains("10de:1f08"));
        // The shared `tests_helpers` profile reports a synthetic
        // model name; assert the PCI ids are present rather than a
        // specific marketing string so the assertion stays
        // future-proof if the helper ever swaps the dummy name.
        assert!(gpu_line.contains("Test"));
        // The shared helper leaves `current_driver` unset so the
        // GPU block reports `driver=none`. The original detection
        // helper hard-coded `Some("nvidia")`; the assertion here
        // mirrors the new shared shape.
        assert!(gpu_line.contains("driver=none"));
        assert!(gpu_line.contains("iommu_group=1"));
        assert!(gpu_line.contains("isolated=true"));

        assert_eq!(view.finding_lines.len(), 0);
        assert_eq!(view.status, CompatibilityStatus::Ready);
    }

    #[test]
    fn detection_view_renders_no_gpu_placeholder_when_profile_has_none() {
        let mut profile = dummy_profile();
        profile.gpus.clear();
        let view = DetectionView::new(&profile, &empty_report(CompatibilityStatus::Ready));
        assert_eq!(view.gpu_lines, vec!["(no GPUs detected)".to_string()]);
    }

    #[test]
    fn detection_view_propagates_finding_severity_and_status() {
        let view = DetectionView::new(&dummy_profile(), &warn_report());
        assert_eq!(view.status, CompatibilityStatus::Warnings);
        assert_eq!(view.finding_lines.len(), 2);
        assert_eq!(view.finding_lines[0].severity, FindingSeverity::Warn);
        assert_eq!(view.finding_lines[1].severity, FindingSeverity::Pass);
        assert!(view.finding_lines[0].text.contains("Secure Boot"));
    }

    #[test]
    fn detection_view_real_compatibility_report_path_works() {
        // Round-trip: build_compatibility_report -> DetectionView.
        // No asserts on specific findings (the fixture profile here
        // is small and has IOMMU disabled etc.); the goal is just to
        // confirm the integration with build_compatibility_report is
        // wired correctly.
        let profile = dummy_profile();
        let report = build_compatibility_report(&profile);
        let view = DetectionView::new(&profile, &report);
        assert_eq!(view.status, report.status);
        assert_eq!(view.finding_lines.len(), report.findings.len());
    }

    #[test]
    fn render_paints_terminal_buffer_without_panicking() {
        // Smoke test: instantiate a TestBackend, render, confirm the
        // buffer was actually written. Catches layout-arithmetic bugs
        // (constraints that don't sum, areas that don't fit) which
        // ratatui will panic on at draw time.
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let view = DetectionView::new(&dummy_profile(), &warn_report());
        terminal
            .draw(|f| {
                let area = f.size();
                render(f, area, &view);
            })
            .expect("render must not panic on a 120x40 terminal");

        // The compatibility-status banner text is rendered into the
        // top row of the buffer. Spot-check it lands somewhere on
        // screen.
        let buffer = terminal.backend().buffer().clone();
        let mut top_text = String::new();
        for x in 0..buffer.area().width {
            top_text.push_str(buffer.get(x, 1).symbol());
        }
        assert!(top_text.contains("Compatibility"));
    }
}

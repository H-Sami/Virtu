// src/detect/audio.rs
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AudioSystem {
    PipeWire,
    PulseAudio,
    Alsa,
    Jack,
    Unknown,
}

impl std::fmt::Display for AudioSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AudioSystem::PipeWire => write!(f, "PipeWire"),
            AudioSystem::PulseAudio => write!(f, "PulseAudio"),
            AudioSystem::Alsa => write!(f, "ALSA"),
            AudioSystem::Jack => write!(f, "JACK"),
            AudioSystem::Unknown => write!(f, "Unknown"),
        }
    }
}

impl AudioSystem {
    pub fn libvirt_audio_type(&self) -> &str {
        match self {
            AudioSystem::PipeWire => "pipewire",
            AudioSystem::PulseAudio => "pulseaudio",
            _ => "none",
        }
    }
}

pub async fn detect() -> Result<AudioSystem> {
    // Check if PipeWire is running and acting as PulseAudio
    let pw = tokio::process::Command::new("pactl")
        .arg("info")
        .output()
        .await
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok());

    if let Some(info) = &pw {
        let parsed = parse_pactl_info(info);
        if parsed != AudioSystem::Unknown {
            return Ok(parsed);
        }
    }
    if std::path::Path::new("/proc/asound").exists() {
        return Ok(AudioSystem::Alsa);
    }
    Ok(AudioSystem::Unknown)
}

pub async fn detect_from_root(root: impl AsRef<Path>) -> Result<AudioSystem> {
    let root = root.as_ref();

    for pactl_fixture in [
        "/run/user/1000/pactl-info",
        "/tmp/virtu-pactl-info",
        "/proc/virtu-pactl-info",
    ] {
        if let Ok(info) = tokio::fs::read_to_string(rooted(root, pactl_fixture)).await {
            let parsed = parse_pactl_info(&info);
            if parsed != AudioSystem::Unknown {
                return Ok(parsed);
            }
        }
    }

    if rooted(root, "/run/user/1000/pipewire-0").exists() {
        return Ok(AudioSystem::PipeWire);
    }
    if rooted(root, "/run/user/1000/pulse/native").exists() {
        return Ok(AudioSystem::PulseAudio);
    }
    if rooted(root, "/proc/asound").exists() {
        return Ok(AudioSystem::Alsa);
    }

    Ok(AudioSystem::Unknown)
}

pub fn parse_pactl_info(info: &str) -> AudioSystem {
    let lower = info.to_lowercase();
    if lower.contains("pipewire") {
        AudioSystem::PipeWire
    } else if lower.contains("pulseaudio") || lower.contains("pulse audio") {
        AudioSystem::PulseAudio
    } else if lower.contains("jack") {
        AudioSystem::Jack
    } else {
        AudioSystem::Unknown
    }
}

fn rooted(root: &Path, absolute_path: &str) -> std::path::PathBuf {
    root.join(absolute_path.trim_start_matches('/'))
}

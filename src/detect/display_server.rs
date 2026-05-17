// detect/display_server.rs
use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DisplayServer {
    Wayland,
    X11,
    Both,
    Tty,
    Unknown,
}

impl std::fmt::Display for DisplayServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DisplayServer::Wayland => write!(f, "Wayland"),
            DisplayServer::X11 => write!(f, "X11"),
            DisplayServer::Both => write!(f, "Wayland + X11"),
            DisplayServer::Tty => write!(f, "TTY only"),
            DisplayServer::Unknown => write!(f, "Unknown"),
        }
    }
}

pub async fn detect() -> Result<DisplayServer> {
    let session = std::env::var("XDG_SESSION_TYPE")
        .unwrap_or_default()
        .to_lowercase();
    match session.as_str() {
        "wayland" => Ok(DisplayServer::Wayland),
        "x11" => Ok(DisplayServer::X11),
        _ => {
            // Check for wayland socket
            let uid = current_uid();
            let wayland_socket =
                std::path::Path::new(&format!("/run/user/{uid}/wayland-0")).exists();
            let x11_socket = std::path::Path::new("/tmp/.X11-unix/X0").exists();
            Ok(match (wayland_socket, x11_socket) {
                (true, true) => DisplayServer::Both,
                (true, false) => DisplayServer::Wayland,
                (false, true) => DisplayServer::X11,
                _ => DisplayServer::Unknown,
            })
        }
    }
}

#[cfg(unix)]
fn current_uid() -> u32 {
    unsafe { libc::getuid() }
}

#[cfg(not(unix))]
fn current_uid() -> u32 {
    1000
}

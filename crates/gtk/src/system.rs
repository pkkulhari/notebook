//! Where the Linux app keeps its files, and what it calls this computer.
use std::path::PathBuf;

pub fn data_path() -> PathBuf {
    xdg_path("XDG_DATA_HOME", ".local/share", "notebook/notebook.db")
}

/// The sync settings, including this device's private key, live apart from
/// the notes, so copying the database never shares the key.
pub fn config_path() -> PathBuf {
    xdg_path("XDG_CONFIG_HOME", ".config", "notebook/sync.json")
}

fn xdg_path(var: &str, fallback: &str, file: &str) -> PathBuf {
    let root = std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(fallback)
        });
    root.join(file)
}

pub fn device_name() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|name| name.trim().to_string())
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "This computer".into())
}

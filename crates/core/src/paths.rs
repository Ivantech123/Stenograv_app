//! Где лежат модели, ffmpeg, настройки и история.

use std::path::PathBuf;

pub const APP_DIR_NAME: &str = "Stenograf";

pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join(APP_DIR_NAME);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn models_dir() -> PathBuf {
    sub("models")
}

pub fn ffmpeg_dir() -> PathBuf {
    sub("ffmpeg")
}

pub fn settings_file() -> PathBuf {
    data_dir().join("settings.json")
}

pub fn history_file() -> PathBuf {
    data_dir().join("history.json")
}

fn sub(name: &str) -> PathBuf {
    let dir = data_dir().join(name);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

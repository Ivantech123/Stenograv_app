//! ffmpeg нужен только для того, что не читает symphonia: WMA, AMR, Opus, видео.
//! Ищем готовый, а если его нет — один раз скачиваем LGPL-сборку (~80 МБ).

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use anyhow::{bail, Context, Result};

use crate::{download, paths};

const ZIP_URL: &str =
    "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-lgpl-shared.zip";

#[cfg(windows)]
const EXE: &str = "ffmpeg.exe";
#[cfg(not(windows))]
const EXE: &str = "ffmpeg";

pub fn find() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(custom) = std::env::var_os("STENOGRAF_FFMPEG") {
        candidates.push(PathBuf::from(custom));
    }
    candidates.push(paths::ffmpeg_dir().join(EXE));
    if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
        candidates.push(dir.join(EXE));
        candidates.push(dir.join("ffmpeg").join(EXE));
        candidates.push(dir.join("tools").join("ffmpeg").join(EXE));
    }
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|dir| dir.join(EXE)));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let winget = PathBuf::from(local).join("Microsoft").join("WinGet").join("Packages");
        candidates.extend(winget_builds(&winget));
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// winget кладёт Gyan.FFmpeg в Packages\Gyan.FFmpeg_*\ffmpeg-*\bin\ffmpeg.exe.
fn winget_builds(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(packages) = std::fs::read_dir(root) else { return found };
    for package in packages.flatten() {
        if !package.file_name().to_string_lossy().starts_with("Gyan.FFmpeg") {
            continue;
        }
        let Ok(builds) = std::fs::read_dir(package.path()) else { continue };
        for build in builds.flatten() {
            found.push(build.path().join("bin").join(EXE));
        }
    }
    found
}

pub fn ensure(cancel: &AtomicBool, on_progress: impl FnMut(u64, Option<u64>)) -> Result<PathBuf> {
    if let Some(path) = find() {
        return Ok(path);
    }
    let dir = paths::ffmpeg_dir();
    let zip_path = dir.join("ffmpeg.zip");
    download::fetch(ZIP_URL, &zip_path, cancel, on_progress)?;
    extract_bin(&zip_path, &dir)?;
    let _ = std::fs::remove_file(&zip_path);
    let exe = dir.join(EXE);
    if !exe.is_file() {
        bail!("После распаковки нет {}", exe.display());
    }
    Ok(exe)
}

/// Shared-сборке рядом с ffmpeg.exe нужны её DLL; ffplay/ffprobe не берём.
fn extract_bin(zip_path: &Path, target: &Path) -> Result<()> {
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file).context("Архив ffmpeg повреждён")?;
    let mut has_exe = false;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name().replace('\\', "/");
        if !name.contains("/bin/") || name.ends_with('/') {
            continue;
        }
        let file_name = name.rsplit('/').next().unwrap_or_default().to_string();
        let keep = file_name.eq_ignore_ascii_case("ffmpeg.exe") || file_name.to_ascii_lowercase().ends_with(".dll");
        if !keep {
            continue;
        }
        has_exe |= file_name.eq_ignore_ascii_case("ffmpeg.exe");
        let mut out = std::fs::File::create(target.join(&file_name))?;
        std::io::copy(&mut entry, &mut out)?;
    }
    if !has_exe {
        bail!("В архиве ffmpeg нет bin/ffmpeg.exe");
    }
    Ok(())
}

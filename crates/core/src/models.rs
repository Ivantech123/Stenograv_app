//! Три режима вместо списка из десяти моделей.
//!
//! На CPU large-v3-turbo быстрее medium (в турбо 4 слоя декодера вместо 24)
//! и заметно точнее на русском, поэтому medium/small не предлагаем вовсе.
//! «Быстро» и «Баланс» — один файл q8: на AVX2 он быстрее q5 (замер: 27 с против 32 с
//! на 46 с записи), режимы отличаются только шириной поиска.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{download, paths};

const HF_WHISPER: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";
const HF_VAD: &str = "https://huggingface.co/ggml-org/whisper-vad/resolve/main";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Preset {
    Fast,
    Balanced,
    Accurate,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelInfo {
    pub preset: Preset,
    pub title: &'static str,
    pub hint: &'static str,
    pub model: &'static str,
    pub file: &'static str,
    pub size: u64,
    /// Ширина луча. 1 — жадный поиск: в разы быстрее, чуть больше ошибок.
    pub beam: i32,
}

pub const CATALOG: [ModelInfo; 3] = [
    ModelInfo {
        preset: Preset::Fast,
        title: "Быстро",
        hint: "Черновик. Та же модель, что «Баланс», но без перебора вариантов",
        model: "large-v3-turbo · q8 · greedy",
        file: "ggml-large-v3-turbo-q8_0.bin",
        size: 874_188_075,
        beam: 1,
    },
    ModelInfo {
        preset: Preset::Balanced,
        title: "Баланс",
        hint: "Лучшее соотношение для русской диктовки",
        model: "large-v3-turbo · q8",
        file: "ggml-large-v3-turbo-q8_0.bin",
        size: 874_188_075,
        beam: 5,
    },
    ModelInfo {
        preset: Preset::Accurate,
        title: "Точно",
        hint: "Полная large-v3: тихая и неразборчивая речь. Нужна видеокарта — на процессоре в 6 раз дольше",
        model: "large-v3 · q5",
        file: "ggml-large-v3-q5_0.bin",
        size: 1_081_140_203,
        beam: 5,
    },
];

pub const VAD_FILE: &str = "ggml-silero-v5.1.2.bin";

impl Preset {
    pub fn info(self) -> &'static ModelInfo {
        CATALOG.iter().find(|m| m.preset == self).expect("every preset is in the catalog")
    }

    pub fn path(self) -> PathBuf {
        paths::models_dir().join(self.info().file)
    }

    pub fn is_installed(self) -> bool {
        installed(&self.path(), Some(self.info().size))
    }

    pub fn url(self) -> String {
        format!("{HF_WHISPER}/{}", self.info().file)
    }
}

pub fn vad_path() -> PathBuf {
    paths::models_dir().join(VAD_FILE)
}

fn installed(path: &std::path::Path, size: Option<u64>) -> bool {
    match std::fs::metadata(path) {
        Ok(meta) => size.is_none_or(|s| meta.len() == s),
        Err(_) => false,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelState {
    #[serde(flatten)]
    pub info: ModelInfo,
    pub installed: bool,
    /// Сколько байт уже лежит в .part от прерванной загрузки.
    pub partial: u64,
}

pub fn states() -> Vec<ModelState> {
    CATALOG
        .iter()
        .map(|info| ModelState {
            info: info.clone(),
            installed: info.preset.is_installed(),
            partial: std::fs::metadata(download::part_path(&info.preset.path())).map(|m| m.len()).unwrap_or(0),
        })
        .collect()
}

/// Модель + VAD. Прогресс считается по сумме обоих файлов.
pub fn ensure(preset: Preset, cancel: &AtomicBool, mut on_progress: impl FnMut(u64, u64)) -> Result<PathBuf> {
    let vad = vad_path();
    let vad_size = 885_098u64;
    let model_size = preset.info().size;
    let total = model_size + vad_size;
    if !installed(&vad, None) {
        download::fetch(&format!("{HF_VAD}/{VAD_FILE}"), &vad, cancel, |done, _| on_progress(done, total))?;
    }
    let path = preset.path();
    if !preset.is_installed() {
        // Файл не того размера — недокачанный или битый: качаем заново.
        let _ = std::fs::remove_file(&path);
        download::fetch(&preset.url(), &path, cancel, |done, _| on_progress(vad_size + done, total))?;
    }
    on_progress(total, total);
    Ok(path)
}

pub fn remove(preset: Preset) -> Result<()> {
    for path in [preset.path(), download::part_path(&preset.path())] {
        if path.exists() {
            std::fs::remove_file(path)?;
        }
    }
    Ok(())
}

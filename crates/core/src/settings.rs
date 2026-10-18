//! Настройки, которые переживают перезапуск.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{models::Preset, paths};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnExists {
    /// «запись (2).txt»: правки в старом тексте не пропадут.
    Rename,
    Overwrite,
    Skip,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Режим подбирается под железо (advisor). Ручной выбор режима выключает.
    pub auto_preset: bool,
    pub preset: Preset,
    /// "ru", "en", … или "auto".
    pub language: String,
    /// 0 — по числу физических ядер.
    pub threads: usize,
    /// Считать на видеокарте, если сборка и железо позволяют (решает advisor).
    pub gpu: bool,
    pub legal: bool,
    /// Имена, номера дел, редкие слова — подсказка модели.
    pub vocabulary: String,
    pub timestamps: bool,
    pub header: bool,
    pub txt: bool,
    pub docx: bool,
    pub srt: bool,
    /// None — рядом с записью.
    pub out_dir: Option<PathBuf>,
    pub on_exists: OnExists,
    pub open_when_done: bool,
    /// "auto", "dark", "light".
    pub theme: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            auto_preset: true,
            preset: Preset::Balanced,
            language: "ru".into(),
            threads: 0,
            gpu: true,
            legal: false,
            vocabulary: String::new(),
            timestamps: true,
            header: true,
            txt: true,
            docx: false,
            srt: false,
            out_dir: None,
            on_exists: OnExists::Rename,
            open_when_done: false,
            theme: "auto".into(),
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        std::fs::read_to_string(paths::settings_file())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = paths::settings_file();
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    /// Хотя бы один формат должен остаться, иначе работа впустую.
    pub fn formats_or_txt(&self) -> (bool, bool, bool) {
        if !self.txt && !self.docx && !self.srt {
            (true, false, false)
        } else {
            (self.txt, self.docx, self.srt)
        }
    }
}

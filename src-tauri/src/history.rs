//! Архив готовых расшифровок: что, когда, где лежит текст.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use stenograf_core::{paths, JobOutcome};

const LIMIT: usize = 300;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub source: PathBuf,
    pub outputs: Vec<PathBuf>,
    pub duration: f64,
    pub words: usize,
    pub preset: String,
    /// Unix-время в миллисекундах.
    pub finished_at: i64,
    pub preview: String,
    /// Вычисляется при чтении: текст ещё на месте?
    #[serde(default, skip_deserializing)]
    pub exists: bool,
}

pub fn load() -> Vec<Entry> {
    let mut list: Vec<Entry> = std::fs::read_to_string(paths::history_file())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    for e in &mut list {
        e.exists = e.outputs.first().is_some_and(|p| p.exists());
    }
    list
}

fn store(list: &[Entry]) {
    if let Ok(json) = serde_json::to_vec(list) {
        let path = paths::history_file();
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(tmp, path);
        }
    }
}

pub fn add(outcome: &JobOutcome, preset: &str) {
    let mut list = load();
    let preview: String = outcome.paragraphs.iter().map(|p| p.text.as_str()).collect::<Vec<_>>().join(" ").chars().take(220).collect();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
    // Повторная расшифровка той же записи заменяет старую строку архива.
    list.retain(|e| e.source != outcome.source);
    list.insert(
        0,
        Entry {
            id: format!("{now:x}"),
            name: outcome.source.file_name().unwrap_or_default().to_string_lossy().into_owned(),
            source: outcome.source.clone(),
            outputs: outcome.outputs.clone(),
            duration: outcome.duration,
            words: outcome.words,
            preset: preset.to_string(),
            finished_at: now,
            preview,
            exists: true,
        },
    );
    list.truncate(LIMIT);
    store(&list);
}

pub fn remove(id: &str) {
    let mut list = load();
    list.retain(|e| e.id != id);
    store(&list);
}

//! Ядро «Стенографа»: запись диктофона → 16 кГц моно → whisper.cpp → текст.
//! Всё считается локально, звук никуда не уходит.

pub mod advisor;
pub mod audio;
pub mod download;
pub mod engine;
pub mod ffmpeg;
pub mod filter;
pub mod hardware;
pub mod job;
pub mod models;
pub mod paths;
pub mod settings;
pub mod text;
pub mod writer;

pub use engine::Engine;
pub use job::{process_file, JobEvent, JobOutcome, Stage};
pub use models::Preset;
pub use settings::Settings;
pub use text::Segment;

/// Пользователь нажал «Стоп».
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Остановлено")
    }
}

impl std::error::Error for Cancelled {}

pub fn is_cancelled(err: &anyhow::Error) -> bool {
    err.downcast_ref::<Cancelled>().is_some()
}

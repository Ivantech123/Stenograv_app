//! Один файл целиком: модель → звук → распознавание → чистка → файлы.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use serde::Serialize;

use crate::engine::{build_prompt, Engine, Request};
use crate::models::{self, Preset};
use crate::settings::Settings;
use crate::text::{self, Paragraph, Segment};
use crate::{audio, ffmpeg, filter, writer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    Model,
    Ffmpeg,
    Decoding,
    Loading,
    Transcribing,
    Saving,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum JobEvent {
    Stage { stage: Stage },
    Download { done: u64, total: u64 },
    Audio { duration: f64, peaks: Vec<f32> },
    Progress { percent: f32 },
    Segment { start: f64, end: f64, text: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct JobOutcome {
    pub source: PathBuf,
    pub outputs: Vec<PathBuf>,
    pub skipped: bool,
    pub duration: f64,
    pub elapsed: f64,
    /// Чистое время распознавания — для замеров скорости этого ПК.
    pub compute: f64,
    pub words: usize,
    pub language: String,
    pub engine: String,
    pub phantoms_removed: usize,
    pub repeats_removed: usize,
    pub paragraphs: Vec<Paragraph>,
}

pub type Emit = Arc<dyn Fn(JobEvent) + Send + Sync>;

pub fn engine_label(preset: Preset, gpu: bool, threads: usize) -> String {
    let device = if gpu { "GPU (Vulkan)".to_string() } else { format!("CPU, потоков: {threads}") };
    format!("whisper.cpp · {} · {device}", preset.info().model)
}

pub fn process_file(
    engine: &mut Engine,
    source: &Path,
    settings: &Settings,
    threads: usize,
    cancel: Arc<AtomicBool>,
    emit: Emit,
) -> Result<JobOutcome> {
    let started = Instant::now();
    let gpu = settings.gpu && cfg!(feature = "gpu");
    let label = engine_label(settings.preset, gpu, threads);

    // Куда писать — решаем заранее: если всё уже есть и стоит «пропускать», не тратим время.
    let (want_txt, want_docx, want_srt) = settings.formats_or_txt();
    let out_dir = settings.out_dir.as_deref();
    let mut targets: Vec<(&str, PathBuf)> = Vec::new();
    for (ext, wanted) in [("txt", want_txt), ("docx", want_docx), ("srt", want_srt)] {
        if wanted {
            if let Some(path) = writer::target(source, out_dir, ext, settings.on_exists) {
                targets.push((ext, path));
            }
        }
    }
    if targets.is_empty() {
        return Ok(JobOutcome {
            source: source.to_path_buf(),
            outputs: Vec::new(),
            skipped: true,
            duration: 0.0,
            elapsed: 0.0,
            compute: 0.0,
            words: 0,
            language: settings.language.clone(),
            engine: label,
            phantoms_removed: 0,
            repeats_removed: 0,
            paragraphs: Vec::new(),
        });
    }

    if !settings.preset.is_installed() || !models::vad_path().is_file() {
        emit(JobEvent::Stage { stage: Stage::Model });
        let e = emit.clone();
        models::ensure(settings.preset, &cancel, move |done, total| e(JobEvent::Download { done, total }))?;
    }

    emit(JobEvent::Stage { stage: Stage::Decoding });
    let e = emit.clone();
    let cancel_ff = cancel.clone();
    let decoded = audio::decode(source, &cancel, move || {
        e(JobEvent::Stage { stage: Stage::Ffmpeg });
        let e2 = e.clone();
        let path = ffmpeg::ensure(&cancel_ff, move |done, total| e2(JobEvent::Download { done, total: total.unwrap_or(0) }))?;
        e(JobEvent::Stage { stage: Stage::Decoding });
        Ok(path)
    })?;
    let duration = decoded.duration();
    emit(JobEvent::Audio { duration, peaks: audio::peaks(&decoded.samples, 360) });

    let mut language = settings.language.clone();
    let mut compute = 0.0;
    let segments: Vec<Segment> = if duration < 0.3 {
        Vec::new()
    } else {
        let model_path = settings.preset.path();
        if !engine.is_loaded(&model_path, gpu) {
            emit(JobEvent::Stage { stage: Stage::Loading });
            engine.load(&model_path, gpu)?;
        }
        emit(JobEvent::Stage { stage: Stage::Transcribing });
        let warm = engine.is_warm();
        let t0 = Instant::now();
        let ep = emit.clone();
        let es = emit.clone();
        let output = engine.transcribe(
            Request {
                samples: &decoded.samples,
                language: &settings.language,
                threads,
                beam: settings.preset.info().beam,
                prompt: build_prompt(settings.legal, &settings.vocabulary),
                vad: Some(models::vad_path()),
            },
            cancel.clone(),
            move |p| ep(JobEvent::Progress { percent: p as f32 }),
            move |s: Segment| es(JobEvent::Segment { start: s.start, end: s.end, text: s.text }),
        )?;
        compute = t0.elapsed().as_secs_f64();
        // Холодный старт на видеокарте (компиляция шейдеров) занижает скорость в разы,
        // кроме совсем длинных записей, где эти секунды теряются.
        if warm || duration > 600.0 {
            crate::advisor::Perf::record(settings.preset, threads, gpu, duration, compute);
        }
        if settings.language == "auto" {
            if let Some(found) = output.language {
                language = found;
            }
        }
        output.segments
    };
    drop(decoded);

    let report = filter::clean(segments);
    emit(JobEvent::Stage { stage: Stage::Saving });
    let meta = writer::Meta {
        source,
        duration,
        engine: &label,
        language: &language,
        timestamps: settings.timestamps,
        header: settings.header,
    };
    let mut outputs = Vec::new();
    for (ext, path) in targets {
        match ext {
            "txt" => writer::write_text(&path, &writer::render_txt(&report.segments, &meta))?,
            "srt" => writer::write_text(&path, &writer::render_srt(&report.segments))?,
            "docx" => writer::write_atomic(&path, |tmp| writer::render_docx(&report.segments, &meta, tmp))?,
            _ => unreachable!(),
        }
        outputs.push(path);
    }

    Ok(JobOutcome {
        source: source.to_path_buf(),
        outputs,
        skipped: false,
        duration,
        elapsed: started.elapsed().as_secs_f64(),
        compute,
        words: text::word_count(&report.segments),
        language,
        engine: label,
        phantoms_removed: report.phantoms_removed,
        repeats_removed: report.repeats_removed,
        paragraphs: writer::plain_text(&report.segments),
    })
}

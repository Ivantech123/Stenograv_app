//! whisper.cpp. Модель грузится один раз и живёт между файлами очереди.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once};

use anyhow::{anyhow, Context, Result};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperVadParams};

use crate::text::Segment;
use crate::Cancelled;

/// Подсказка, а не замена: модель лишь охотнее пишет эти слова так, как здесь.
pub const LEGAL_TERMS: &str = "Истец, ответчик, третье лицо, суд, судья, ГК РФ, ГПК РФ, АПК РФ, КоАП РФ, УК РФ, \
исковое заявление, ходатайство, определение, решение, апелляция, кассация, госпошлина, неустойка, убытки, \
исковая давность, обеспечительные меры, мировое соглашение, доверенность, нотариус, протокол судебного заседания.";

pub struct Request<'a> {
    pub samples: &'a [f32],
    /// "ru", "en", … или "auto".
    pub language: &'a str,
    pub threads: usize,
    pub beam: i32,
    pub prompt: Option<String>,
    pub vad: Option<PathBuf>,
}

pub struct Output {
    pub segments: Vec<Segment>,
    pub language: Option<String>,
}

struct Loaded {
    path: PathBuf,
    gpu: bool,
    ctx: WhisperContext,
    /// Сколько файлов уже прошло через эту модель. Первый на видеокарте
    /// платит за компиляцию шейдеров — его скорость в замеры не берём.
    runs: u32,
}

#[derive(Default)]
pub struct Engine {
    loaded: Option<Loaded>,
}

impl Engine {
    pub fn new() -> Self {
        static QUIET: Once = Once::new();
        // Без этого whisper.cpp сыплет отладкой в stderr каждого запуска.
        QUIET.call_once(whisper_rs::install_logging_hooks);
        Self::default()
    }

    pub fn is_loaded(&self, path: &Path, gpu: bool) -> bool {
        self.loaded.as_ref().is_some_and(|l| l.path == path && l.gpu == gpu)
    }

    pub fn load(&mut self, path: &Path, gpu: bool) -> Result<()> {
        if self.is_loaded(path, gpu) {
            return Ok(());
        }
        self.loaded = None; // освобождаем память старой модели до загрузки новой
        let mut params = WhisperContextParameters::default();
        // Flash attention ускоряет видеокарту, а на CPU замедляет (замер: 19,2 с против 15,7 с на окно).
        params.use_gpu(gpu).flash_attn(gpu);
        let ctx = WhisperContext::new_with_params(path, params)
            .map_err(|e| anyhow!("{e:?}"))
            .with_context(|| format!("Модель не загрузилась: {}. Удалите её на вкладке «Модели» и скачайте заново", path.display()))?;
        self.loaded = Some(Loaded { path: path.to_path_buf(), gpu, ctx, runs: 0 });
        Ok(())
    }

    /// Модель уже считала хотя бы один файл — замер скорости будет честным.
    pub fn is_warm(&self) -> bool {
        self.loaded.as_ref().is_some_and(|l| !l.gpu || l.runs > 0)
    }

    pub fn unload(&mut self) {
        self.loaded = None;
    }

    pub fn transcribe(
        &mut self,
        req: Request<'_>,
        cancel: Arc<AtomicBool>,
        on_progress: impl FnMut(i32) + 'static,
        on_segment: impl FnMut(Segment) + 'static,
    ) -> Result<Output> {
        let loaded = self.loaded.as_ref().ok_or_else(|| anyhow!("Модель не загружена"))?;
        let mut state = loaded.ctx.create_state().map_err(|e| anyhow!("whisper: {e:?}"))?;

        let strategy = if req.beam > 1 {
            SamplingStrategy::BeamSearch { beam_size: req.beam, patience: -1.0 }
        } else {
            SamplingStrategy::Greedy { best_of: 1 }
        };
        let mut params = FullParams::new(strategy);
        params.set_n_threads(req.threads.max(1) as i32);
        params.set_language(Some(req.language));
        params.set_translate(false);
        // Как condition_on_previous_text=False: ошибка одного окна не тянется в следующие.
        params.set_no_context(true);
        params.set_suppress_blank(true);
        params.set_suppress_nst(true);
        params.set_temperature(0.0);
        params.set_temperature_inc(0.2);
        params.set_entropy_thold(2.4);
        params.set_logprob_thold(-1.0);
        params.set_no_speech_thold(0.6);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        if let Some(prompt) = req.prompt.as_deref().filter(|p| !p.trim().is_empty()) {
            params.set_initial_prompt(prompt);
        }
        let vad_path = req.vad.as_ref().filter(|p| p.is_file()).map(|p| p.to_string_lossy().into_owned());
        if let Some(vad) = vad_path.as_deref() {
            // Silero вырезает тишину и шум до модели: меньше галлюцинаций и быстрее.
            params.set_vad_model_path(Some(vad));
            let mut vad_params = WhisperVadParams::new();
            vad_params.set_min_silence_duration(500);
            vad_params.set_speech_pad(280);
            params.set_vad_params(vad_params);
            params.enable_vad(true);
        }

        let mut on_progress = on_progress;
        params.set_progress_callback_safe(move |p: i32| on_progress(p));
        let mut on_segment = on_segment;
        params.set_segment_callback_safe(move |data: whisper_rs::SegmentCallbackData| {
            let text = data.text.trim();
            if !text.is_empty() {
                on_segment(Segment::new(data.start_timestamp as f64 / 100.0, data.end_timestamp as f64 / 100.0, text));
            }
        });
        let flag = cancel.clone();
        // Тип F указан явно: обёртка whisper-rs приводит user_data именно к Box<dyn FnMut>.
        let abort: Box<dyn FnMut() -> bool> = Box::new(move || flag.load(Ordering::Relaxed));
        params.set_abort_callback_safe::<_, Box<dyn FnMut() -> bool>>(Some(abort));

        let result = state.full(params, req.samples);
        if let Some(l) = self.loaded.as_mut() {
            l.runs += 1;
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(Cancelled.into());
        }
        result.map_err(|e| anyhow!("Распознавание прервалось: {e:?}"))?;

        let mut segments = Vec::new();
        for seg in state.as_iter() {
            let text = seg.to_str_lossy().map(|t| t.trim().to_string()).unwrap_or_default();
            if text.is_empty() {
                continue;
            }
            let mut s = Segment::new(seg.start_timestamp() as f64 / 100.0, seg.end_timestamp() as f64 / 100.0, text);
            s.no_speech = seg.no_speech_probability();
            segments.push(s);
        }
        let language = whisper_rs::get_lang_str(state.full_lang_id_from_state()).map(str::to_string);
        Ok(Output { segments, language })
    }
}

/// Юридический словарь + свои слова пользователя. Whisper читает
/// не больше ~220 токенов подсказки, поэтому режем по длине.
pub fn build_prompt(legal: bool, vocabulary: &str) -> Option<String> {
    let mut parts = Vec::new();
    if legal {
        parts.push(LEGAL_TERMS.to_string());
    }
    let words: Vec<&str> = vocabulary
        .split([',', ';', '\n'])
        .map(str::trim)
        .filter(|w| !w.is_empty())
        .collect();
    if !words.is_empty() {
        let mut own = words.join(", ");
        if !own.ends_with('.') {
            own.push('.');
        }
        // Свои слова важнее общего словаря: ставим их последними, ближе к началу речи.
        parts.push(own);
    }
    if parts.is_empty() {
        return None;
    }
    let joined = parts.join(" ");
    let chars: Vec<char> = joined.chars().collect();
    const LIMIT: usize = 600;
    Some(if chars.len() > LIMIT { chars[chars.len() - LIMIT..].iter().collect() } else { joined })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_from_vocabulary() {
        assert_eq!(build_prompt(false, "  "), None);
        assert_eq!(build_prompt(false, "Иванов; ООО «Ромашка»\nдело А40-1234"), Some("Иванов, ООО «Ромашка», дело А40-1234.".into()));
        assert!(build_prompt(true, "Петров").unwrap().ends_with("Петров."));
    }
}

//! Любая запись → f32, 16 кГц, моно. Это вход whisper.cpp.
//!
//! WAV/MP3/FLAC/OGG/M4A декодируются внутри программы (symphonia + rubato).
//! WMA, AMR, Opus и видео — через ffmpeg, который найдётся или скачается.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{anyhow, bail, Context, Result};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::Cancelled;

pub const SAMPLE_RATE: u32 = 16_000;

pub const EXTENSIONS: &[&str] = &[
    "wma", "asf", "wmv", "wav", "wave", "mp3", "m4a", "m4b", "aac", "ogg", "oga", "opus", "flac", "amr", "3gp",
    "aiff", "aif", "caf", "mp4", "mov", "webm", "mkv",
];

/// Что symphonia гарантированно читает. Остальное сразу отдаём ffmpeg.
const NATIVE: &[&str] = &["wav", "wave", "mp3", "flac", "ogg", "oga", "m4a", "m4b", "aac", "aiff", "aif", "caf", "mp4"];

pub fn is_audio(path: &Path) -> bool {
    ext(path).is_some_and(|e| EXTENSIONS.contains(&e.as_str()))
}

fn ext(path: &Path) -> Option<String> {
    path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase())
}

/// Раскрывает папки (рекурсивно) и оставляет только звук, порядок — по имени.
pub fn collect(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for path in paths {
        if path.is_dir() {
            let mut inner = Vec::new();
            walk(path, &mut inner);
            inner.sort();
            out.extend(inner);
        } else if path.is_file() && is_audio(path) {
            out.push(path.clone());
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|p| seen.insert(p.clone()));
    out
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else if is_audio(&path) {
            out.push(path);
        }
    }
}

pub struct Decoded {
    pub samples: Vec<f32>,
    pub via_ffmpeg: bool,
}

impl Decoded {
    pub fn duration(&self) -> f64 {
        self.samples.len() as f64 / SAMPLE_RATE as f64
    }
}

/// `need_ffmpeg` вызывается, только если ffmpeg действительно понадобился и его нет.
pub fn decode(path: &Path, cancel: &AtomicBool, need_ffmpeg: impl FnOnce() -> Result<PathBuf>) -> Result<Decoded> {
    if !path.is_file() {
        bail!("Файл не найден: {}", path.display());
    }
    let native = ext(path).is_some_and(|e| NATIVE.contains(&e.as_str()));
    let mut native_error = None;
    if native {
        match decode_native(path, cancel) {
            Ok(samples) if !samples.is_empty() => return Ok(Decoded { samples, via_ffmpeg: false }),
            Ok(_) => native_error = Some(anyhow!("в файле нет звука")),
            Err(err) if crate::is_cancelled(&err) => return Err(err),
            Err(err) => native_error = Some(err),
        }
    }
    let ffmpeg = match crate::ffmpeg::find() {
        Some(path) => path,
        None => need_ffmpeg()?,
    };
    decode_ffmpeg(&ffmpeg, path, cancel)
        .map(|samples| Decoded { samples, via_ffmpeg: true })
        .map_err(|err| match native_error {
            Some(native) if !crate::is_cancelled(&err) => err.context(format!("встроенный декодер: {native:#}")),
            _ => err,
        })
}

fn decode_native(path: &Path, cancel: &AtomicBool) -> Result<Vec<f32>> {
    let file = std::fs::File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(e) = ext(path) {
        hint.with_extension(&e);
    }
    let probed =
        symphonia::default::get_probe().format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| anyhow!("нет звуковой дорожки"))?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())?;
    let mut rate = track.codec_params.sample_rate.unwrap_or(0);

    let mut mono: Vec<f32> = Vec::new();
    let mut buffer: Option<SampleBuffer<f32>> = None;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Cancelled.into());
        }
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => break,
            Err(e) => return Err(e.into()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            // Битый пакет посреди диктовки — пропускаем, а не бросаем всю запись.
            Err(SymError::DecodeError(_)) | Err(SymError::IoError(_)) => continue,
            Err(e) => return Err(e.into()),
        };
        let spec = *decoded.spec();
        rate = spec.rate;
        let channels = spec.channels.count().max(1);
        let frames = decoded.capacity() as u64;
        let fits = buffer.as_ref().is_some_and(|b| b.capacity() as u64 >= frames * channels as u64);
        if !fits {
            buffer = Some(SampleBuffer::new(frames, spec));
        }
        let buf = buffer.as_mut().expect("buffer set above");
        buf.copy_interleaved_ref(decoded);
        let inv = 1.0 / channels as f32;
        mono.extend(buf.samples().chunks_exact(channels).map(|frame| frame.iter().sum::<f32>() * inv));
    }
    if rate == 0 {
        bail!("неизвестная частота дискретизации");
    }
    resample(mono, rate, cancel)
}

fn resample(input: Vec<f32>, from: u32, cancel: &AtomicBool) -> Result<Vec<f32>> {
    use rubato::{FftFixedIn, Resampler};
    if from == SAMPLE_RATE || input.is_empty() {
        return Ok(input);
    }
    let mut resampler = FftFixedIn::<f32>::new(from as usize, SAMPLE_RATE as usize, 4096, 2, 1)?;
    let delay = resampler.output_delay();
    let expected = (input.len() as u64 * SAMPLE_RATE as u64 / from as u64) as usize;
    let mut out = Vec::with_capacity(expected + 8192);
    let mut pos = 0;
    while pos + resampler.input_frames_next() <= input.len() {
        if cancel.load(Ordering::Relaxed) {
            return Err(Cancelled.into());
        }
        let need = resampler.input_frames_next();
        let block = resampler.process(&[&input[pos..pos + need]], None)?;
        out.extend_from_slice(&block[0]);
        pos += need;
    }
    if pos < input.len() {
        let block = resampler.process_partial(Some(&[&input[pos..]]), None)?;
        out.extend_from_slice(&block[0]);
    }
    // Хвост, застрявший в задержке фильтра.
    let tail = resampler.process_partial::<&[f32]>(None, None)?;
    out.extend_from_slice(&tail[0]);
    let start = delay.min(out.len());
    let end = (start + expected).min(out.len());
    Ok(out[start..end].to_vec())
}

fn decode_ffmpeg(ffmpeg: &Path, source: &Path, cancel: &AtomicBool) -> Result<Vec<f32>> {
    let mut command = Command::new(ffmpeg);
    command
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-i"])
        .arg(source)
        .args(["-vn", "-ac", "1", "-ar", "16000", "-f", "f32le", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    no_window(&mut command);
    let mut child = command.spawn().with_context(|| format!("Не запустился ffmpeg: {}", ffmpeg.display()))?;

    let mut stderr = child.stderr.take().expect("stderr piped");
    let errors = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });

    let mut stdout = child.stdout.take().expect("stdout piped");
    let mut bytes = Vec::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Cancelled.into());
        }
        let n = stdout.read(&mut buf)?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&buf[..n]);
    }
    let status = child.wait()?;
    let detail = errors.join().unwrap_or_default();
    if !status.success() || bytes.is_empty() {
        let name = source.file_name().unwrap_or_default().to_string_lossy();
        let detail = detail.trim();
        if detail.is_empty() {
            bail!("ffmpeg не смог прочитать «{name}»");
        }
        bail!("ffmpeg не смог прочитать «{name}»: {detail}");
    }
    Ok(bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect())
}

#[cfg(windows)]
pub fn no_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
pub fn no_window(_command: &mut Command) {}

/// Огибающая для картинки в окне: `buckets` значений от 0 до 1.
pub fn peaks(samples: &[f32], buckets: usize) -> Vec<f32> {
    if samples.is_empty() || buckets == 0 {
        return vec![0.0; buckets];
    }
    let size = samples.len().div_ceil(buckets).max(1);
    let mut out: Vec<f32> = samples
        .chunks(size)
        .map(|chunk| {
            // RMS ровнее пика: щелчок диктофона не рисует столб на всю высоту.
            let sum: f32 = chunk.iter().map(|s| s * s).sum();
            (sum / chunk.len() as f32).sqrt()
        })
        .collect();
    out.resize(buckets, 0.0);
    let max = out.iter().cloned().fold(0.0f32, f32::max);
    if max > 0.0 {
        for v in &mut out {
            *v = (*v / max).sqrt();
        }
    }
    out
}

//! stenograf запись.wma
//! stenograf D:\диктофон --out D:\тексты --preset accurate --legal --docx
//! stenograf --doctor

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use clap::{Parser, ValueEnum};
use stenograf_core::settings::OnExists;
use stenograf_core::text::{clock, human_duration};
use stenograf_core::{advisor, audio, ffmpeg, hardware, models, process_file, Engine, JobEvent, Preset, Settings, Stage};

#[derive(Parser)]
#[command(name = "stenograf", version, about = "Локальная расшифровка диктофонных записей. Звук не уходит в сеть.")]
struct Cli {
    /// Файлы или папки с записями
    paths: Vec<PathBuf>,
    /// Папка для результатов. По умолчанию — рядом с записью
    #[arg(long)]
    out: Option<PathBuf>,
    #[arg(long, value_enum)]
    preset: Option<PresetArg>,
    /// ru, en, … или auto
    #[arg(long)]
    language: Option<String>,
    /// Потоки CPU. По умолчанию — физические ядра
    #[arg(long)]
    threads: Option<usize>,
    /// Подсказать модели юридические термины
    #[arg(long)]
    legal: bool,
    /// Имена, номера дел, редкие слова через запятую
    #[arg(long)]
    words: Option<String>,
    #[arg(long)]
    docx: bool,
    #[arg(long)]
    srt: bool,
    #[arg(long)]
    no_timestamps: bool,
    #[arg(long)]
    no_header: bool,
    /// Что делать, если результат уже есть
    #[arg(long, value_enum, default_value = "rename")]
    existing: ExistingArg,
    /// Показать процессор, модели и ffmpeg
    #[arg(long)]
    doctor: bool,
    /// Время каждого этапа
    #[arg(long, short)]
    verbose: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum PresetArg {
    Fast,
    Balanced,
    Accurate,
}

#[derive(Clone, Copy, ValueEnum)]
enum ExistingArg {
    Rename,
    Overwrite,
    Skip,
}

fn main() {
    let code = match run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("Ошибка: {err:#}");
            1
        }
    };
    std::process::exit(code);
}

fn run() -> Result<i32> {
    let cli = Cli::parse();
    let hw = hardware::inspect();
    if cli.doctor {
        doctor(&hw);
        if cli.paths.is_empty() {
            return Ok(0);
        }
    }
    let files = audio::collect(&cli.paths);
    if files.is_empty() {
        eprintln!("Нет звуковых файлов. Пример: stenograf запись.wma");
        return Ok(2);
    }

    // Настройки окна — основа, ключи командной строки поверх.
    let mut s = Settings::load();
    let advice = advisor::advise(&hw, s.gpu, cli.threads.unwrap_or(s.threads));
    if let Some(block) = &advice.blocker {
        eprintln!("{block}");
        return Ok(1);
    }
    if cli.preset.is_none() && s.auto_preset {
        s.preset = advice.preset;
        println!("Режим «{}» подобран под этот ПК: {}", s.preset.info().title, advice.reasons.join("; "));
    }
    if let Some(p) = cli.preset {
        s.preset = match p {
            PresetArg::Fast => Preset::Fast,
            PresetArg::Balanced => Preset::Balanced,
            PresetArg::Accurate => Preset::Accurate,
        };
    }
    if let Some(lang) = cli.language {
        s.language = lang;
    }
    s.legal |= cli.legal;
    if let Some(words) = cli.words {
        s.vocabulary = words;
    }
    s.txt = true;
    s.docx = cli.docx;
    s.srt = cli.srt;
    s.timestamps = !cli.no_timestamps;
    s.header = !cli.no_header;
    s.out_dir = cli.out;
    s.on_exists = match cli.existing {
        ExistingArg::Rename => OnExists::Rename,
        ExistingArg::Overwrite => OnExists::Overwrite,
        ExistingArg::Skip => OnExists::Skip,
    };
    let threads = advice.threads;

    let cancel = Arc::new(AtomicBool::new(false));
    {
        let cancel = cancel.clone();
        let _ = ctrlc_hook(move || cancel.store(true, Ordering::Relaxed));
    }

    let mut engine = Engine::new();
    let mut failed = 0;
    for (i, file) in files.iter().enumerate() {
        println!("\n[{}/{}] {}", i + 1, files.len(), file.display());
        let t0 = std::time::Instant::now();
        let verbose = cli.verbose;
        let emit: stenograf_core::job::Emit = Arc::new(move |event| match event {
            JobEvent::Stage { stage } => {
                if verbose {
                    print!("  {:>6.2} с ", t0.elapsed().as_secs_f64());
                }
                let text = match stage {
                    Stage::Model => "Скачиваю модель (один раз)…",
                    Stage::Ffmpeg => "Скачиваю ffmpeg для этого формата (один раз)…",
                    Stage::Decoding => "Читаю запись…",
                    Stage::Loading => "Загружаю модель в память…",
                    Stage::Transcribing => "Распознаю…",
                    Stage::Saving => "Сохраняю…",
                };
                println!("  {text}");
            }
            JobEvent::Download { done, total } => {
                if total > 0 {
                    print!("\r  {:>5.1}%  {} / {} МБ", done as f64 * 100.0 / total as f64, done >> 20, total >> 20);
                    let _ = std::io::stdout().flush();
                    if done >= total {
                        println!();
                    }
                }
            }
            JobEvent::Audio { duration, .. } => println!("  Длительность: {}", human_duration(duration)),
            JobEvent::Segment { start, text, .. } => println!("  [{}] {text}", clock(start)),
            JobEvent::Progress { .. } => {}
        });
        match process_file(&mut engine, file, &s, threads, cancel.clone(), emit) {
            Ok(out) if out.skipped => println!("  Уже расшифрован, пропускаю."),
            Ok(out) => {
                let speed = if out.elapsed > 0.0 { out.duration / out.elapsed } else { 0.0 };
                println!("  Готово за {} (×{speed:.1} к реальному времени), слов: {}", human_duration(out.elapsed), out.words);
                for path in out.outputs {
                    println!("  → {}", path.display());
                }
            }
            Err(err) if stenograf_core::is_cancelled(&err) => {
                println!("  Остановлено.");
                return Ok(130);
            }
            Err(err) => {
                failed += 1;
                println!("  Ошибка: {err:#}");
            }
        }
    }
    println!("\nИтог: готово {}, с ошибкой {failed}.", files.len() - failed);
    Ok(if failed > 0 { 1 } else { 0 })
}

fn doctor(hw: &hardware::HardwareInfo) {
    let gb = |b: u64| format!("{:.1} ГБ", b as f64 / (1u64 << 30) as f64);
    println!("Процессор: {}", hw.cpu);
    println!(
        "Ядра: {} физических ({} производительных, {} энергоэффективных), {} логических",
        hw.physical_cores, hw.performance_cores, hw.efficiency_cores, hw.logical_cores
    );
    println!("AVX2: {}, FMA: {}, AVX-512: {}", yes(hw.avx2), yes(hw.fma), yes(hw.avx512));
    println!("Память: {} всего, {} свободно", gb(hw.ram_total), gb(hw.ram_available));
    for g in &hw.gpus {
        let vram = if g.vram > 0 { gb(g.vram) } else { "память не сообщает".into() };
        println!("Видеокарта: {} ({vram}, {})", g.name, if g.integrated { "встроенная" } else { "дискретная" });
    }
    println!("NPU: {}", hw.npu.as_deref().unwrap_or("нет"));
    match (hw.power.on_battery, hw.power.battery_percent) {
        (true, Some(p)) => println!("Питание: батарея, {p}%"),
        (false, Some(p)) => println!("Питание: от сети, батарея {p}%"),
        _ => println!("Питание: от сети"),
    }
    if let Some(free) = hw.disk_free {
        println!("Свободно на диске моделей: {}", gb(free));
    }
    println!("Сборка с GPU (Vulkan): {}", yes(hw.gpu_build));
    println!("whisper.cpp: {}", hw.whisper_cpp);
    println!("ffmpeg: {}", ffmpeg::find().map(|p| p.display().to_string()).unwrap_or_else(|| "не найден, скачается при первой WMA".into()));
    for m in models::states() {
        println!("Модель «{}» ({}): {}", m.info.title, m.info.model, if m.installed { "скачана" } else { "не скачана" });
    }
    let advice = advisor::advise(hw, Settings::load().gpu, 0);
    println!();
    for e in &advice.estimates {
        println!(
            "  {:<7} ×{:>5.1} к реальному времени{}{}",
            e.preset.info().title,
            e.speed,
            if e.measured { " (замер)" } else { " (оценка)" },
            if e.fits_ram { "" } else { " — не хватит памяти" }
        );
    }
    println!("Рекомендуемый режим: «{}», потоков: {}", advice.preset.info().title, advice.threads);
    for r in &advice.reasons {
        println!("  · {r}");
    }
    if let Some(b) = advice.blocker {
        println!("  ! {b}");
    }
}

fn yes(v: bool) -> &'static str {
    if v { "да" } else { "нет" }
}

/// Ctrl+C: остановить после текущего шага, а не убить процесс посреди записи файла.
#[cfg(windows)]
fn ctrlc_hook(f: impl Fn() + Send + Sync + 'static) -> Result<()> {
    use std::sync::OnceLock;
    static HANDLER: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();
    let _ = HANDLER.set(Box::new(f));
    unsafe extern "system" fn on_ctrl(_kind: u32) -> i32 {
        if let Some(h) = HANDLER.get() {
            h();
        }
        1
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn SetConsoleCtrlHandler(handler: Option<unsafe extern "system" fn(u32) -> i32>, add: i32) -> i32;
    }
    unsafe { SetConsoleCtrlHandler(Some(on_ctrl), 1) };
    Ok(())
}

#[cfg(not(windows))]
fn ctrlc_hook(_f: impl Fn() + Send + Sync + 'static) -> Result<()> {
    Ok(())
}

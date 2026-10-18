//! Оболочка окна: очередь файлов, один поток распознавания, события в интерфейс.

mod history;
mod system;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use serde::Serialize;
use serde_json::json;
use stenograf_core::job::Emit;
use stenograf_core::{advisor, audio, ffmpeg, hardware, models, process_file, Engine, JobEvent, Preset, Settings, Stage};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_opener::OpenerExt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Status {
    Waiting,
    Running,
    Done,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize)]
struct Job {
    id: u64,
    name: String,
    path: PathBuf,
    status: Status,
    error: Option<String>,
    skipped: bool,
}

#[derive(Default)]
struct Shared {
    queue: Mutex<Vec<Job>>,
    wake: Condvar,
    /// Флаг отмены текущего файла.
    current: Mutex<Option<Arc<AtomicBool>>>,
    next_id: AtomicU64,
    settings: Mutex<Settings>,
    /// Одна загрузка модели за раз: вкладка «Модели» и очередь не пишут в один .part.
    download: Mutex<()>,
    model_cancel: Mutex<Option<Arc<AtomicBool>>>,
}

type State = Arc<Shared>;

fn emit_queue(app: &AppHandle, shared: &Shared) {
    let q = shared.queue.lock().unwrap().clone();
    let _ = app.emit("queue", q);
}

fn toast(app: &AppHandle, text: impl Into<String>, error: bool) {
    let _ = app.emit("toast", json!({ "text": text.into(), "error": error }));
}

// ───────────────────────── Команды для окна ─────────────────────────

#[tauri::command]
fn bootstrap(state: tauri::State<State>) -> serde_json::Value {
    let settings = state.settings.lock().unwrap().clone();
    let hw = hardware::inspect();
    let advice = advisor::advise(&hw, settings.gpu, settings.threads);
    json!({
        "settings": settings,
        "hardware": hw,
        "advice": advice,
        "models": models::states().into_iter().map(model_json).collect::<Vec<_>>(),
        "ffmpeg": ffmpeg::find(),
        "queue": state.queue.lock().unwrap().clone(),
        "history": history::load(),
        "version": env!("CARGO_PKG_VERSION"),
    })
}

fn model_json(m: models::ModelState) -> serde_json::Value {
    let mut v = serde_json::to_value(&m).unwrap_or_default();
    v["preset"] = serde_json::to_value(m.info.preset).unwrap_or_default();
    v
}

#[tauri::command]
fn advice(state: tauri::State<State>) -> advisor::Advice {
    let s = state.settings.lock().unwrap().clone();
    advisor::advise(&hardware::inspect(), s.gpu, s.threads)
}

#[tauri::command]
fn save_settings(settings: Settings, state: tauri::State<State>) -> Result<(), String> {
    settings.save().map_err(|e| format!("Настройки не сохранились: {e:#}"))?;
    *state.settings.lock().unwrap() = settings;
    Ok(())
}

#[tauri::command]
fn enqueue(paths: Vec<PathBuf>, app: AppHandle, state: tauri::State<State>) -> usize {
    add_files(&app, &state, &paths)
}

fn add_files(app: &AppHandle, shared: &Shared, paths: &[PathBuf]) -> usize {
    let files = audio::collect(paths);
    let mut q = shared.queue.lock().unwrap();
    let mut added = 0;
    for path in files {
        // Тот же файл уже ждёт или идёт — второй раз не ставим.
        if q.iter().any(|j| j.path == path && matches!(j.status, Status::Waiting | Status::Running)) {
            continue;
        }
        let id = shared.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        q.push(Job { id, name, path, status: Status::Waiting, error: None, skipped: false });
        added += 1;
    }
    drop(q);
    if added > 0 {
        shared.wake.notify_all();
        emit_queue(app, shared);
    }
    added
}

#[tauri::command]
fn remove_job(id: u64, app: AppHandle, state: tauri::State<State>) {
    state.queue.lock().unwrap().retain(|j| j.id != id || j.status == Status::Running);
    emit_queue(&app, &state);
}

/// «Остановить»: текущий файл прерывается, ожидающие снимаются.
#[tauri::command]
fn stop(app: AppHandle, state: tauri::State<State>) {
    if let Some(flag) = state.current.lock().unwrap().as_ref() {
        flag.store(true, Ordering::Relaxed);
    }
    for j in state.queue.lock().unwrap().iter_mut() {
        if j.status == Status::Waiting {
            j.status = Status::Cancelled;
        }
    }
    emit_queue(&app, &state);
}

#[tauri::command]
fn clear_finished(app: AppHandle, state: tauri::State<State>) {
    state.queue.lock().unwrap().retain(|j| matches!(j.status, Status::Waiting | Status::Running));
    emit_queue(&app, &state);
}

#[tauri::command]
fn model_download(preset: Preset, app: AppHandle, state: tauri::State<State>) {
    let shared = state.inner().clone();
    std::thread::spawn(move || {
        let Ok(_lock) = shared.download.try_lock() else {
            toast(&app, "Модель уже скачивается", false);
            return;
        };
        let cancel = Arc::new(AtomicBool::new(false));
        *shared.model_cancel.lock().unwrap() = Some(cancel.clone());
        let a = app.clone();
        let result = models::ensure(preset, &cancel, move |done, total| {
            let _ = a.emit("model-progress", json!({ "preset": preset, "done": done, "total": total }));
        });
        *shared.model_cancel.lock().unwrap() = None;
        let _ = app.emit("model-progress", serde_json::Value::Null);
        match result {
            Ok(_) => toast(&app, format!("Модель «{}» готова", preset.info().title), false),
            Err(e) if stenograf_core::is_cancelled(&e) => toast(&app, "Загрузка остановлена — продолжится с того же места", false),
            Err(e) => toast(&app, format!("{e:#}"), true),
        }
        emit_models(&app);
    });
}

#[tauri::command]
fn model_cancel(state: tauri::State<State>) {
    if let Some(flag) = state.model_cancel.lock().unwrap().as_ref() {
        flag.store(true, Ordering::Relaxed);
    }
}

#[tauri::command]
fn model_delete(preset: Preset, app: AppHandle, state: tauri::State<State>) -> Result<(), String> {
    let busy = state.queue.lock().unwrap().iter().any(|j| j.status == Status::Running);
    if busy {
        return Err("Сейчас идёт расшифровка — удалите модель после неё".into());
    }
    models::remove(preset).map_err(|e| e.to_string())?;
    emit_models(&app);
    Ok(())
}

fn emit_models(app: &AppHandle) {
    let _ = app.emit("models", models::states().into_iter().map(model_json).collect::<Vec<_>>());
}

#[tauri::command]
fn open_path(path: PathBuf, app: AppHandle) -> Result<(), String> {
    app.opener().open_path(path.to_string_lossy(), None::<&str>).map_err(|e| e.to_string())
}

#[tauri::command]
fn reveal(path: PathBuf, app: AppHandle) -> Result<(), String> {
    app.opener().reveal_item_in_dir(path).map_err(|e| e.to_string())
}

#[tauri::command]
fn history_list() -> Vec<history::Entry> {
    history::load()
}

#[tauri::command]
fn history_remove(id: String, app: AppHandle) {
    history::remove(&id);
    let _ = app.emit("history", history::load());
}

#[tauri::command]
fn taskbar(progress: Option<u64>, app: AppHandle) {
    use tauri::window::{ProgressBarState, ProgressBarStatus};
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.set_progress_bar(match progress {
            Some(p) => ProgressBarState { status: Some(ProgressBarStatus::Normal), progress: Some(p.min(100)) },
            None => ProgressBarState { status: Some(ProgressBarStatus::None), progress: None },
        });
    }
}

// ───────────────────────── Поток распознавания ─────────────────────────

fn worker(app: AppHandle, shared: State) {
    let mut engine = Engine::new();
    loop {
        // Ждём следующий файл.
        let job = {
            let mut q = shared.queue.lock().unwrap();
            loop {
                if let Some(j) = q.iter_mut().find(|j| j.status == Status::Waiting) {
                    j.status = Status::Running;
                    break j.clone();
                }
                system::busy(false);
                q = shared.wake.wait(q).unwrap();
            }
        };
        emit_queue(&app, &shared);
        system::busy(true);

        let cancel = Arc::new(AtomicBool::new(false));
        *shared.current.lock().unwrap() = Some(cancel.clone());

        // Настройки — снимок на момент старта файла; режим подбирается под ПК заново:
        // за час могли отключить зарядку или занять память.
        let mut settings = shared.settings.lock().unwrap().clone();
        let hw = hardware::inspect();
        let advice = advisor::advise(&hw, settings.gpu, settings.threads);
        if settings.auto_preset {
            settings.preset = advice.preset;
        }
        settings.gpu = advice.gpu;

        let id = job.id;
        let a = app.clone();
        let emit: Emit = Arc::new(move |event: JobEvent| {
            let _ = a.emit("job", json!({ "id": id, "event": event }));
        });

        let result = (|| {
            if let Some(block) = &advice.blocker {
                anyhow::bail!("{block}");
            }
            // Модель качаем под общим замком — вдруг её же качает вкладка «Модели».
            if !settings.preset.is_installed() || !models::vad_path().is_file() {
                emit(JobEvent::Stage { stage: Stage::Model });
                let _lock = shared.download.lock().unwrap();
                let e = emit.clone();
                models::ensure(settings.preset, &cancel, move |done, total| e(JobEvent::Download { done, total }))?;
                emit_models(&app);
            }
            process_file(&mut engine, &job.path, &settings, advice.threads, cancel.clone(), emit.clone())
        })();
        *shared.current.lock().unwrap() = None;

        let (status, error, skipped, outcome) = match result {
            Ok(o) => (Status::Done, None, o.skipped, Some(o)),
            Err(e) if stenograf_core::is_cancelled(&e) => (Status::Cancelled, None, false, None),
            Err(e) => (Status::Failed, Some(format!("{e:#}")), false, None),
        };
        if let Some(o) = outcome.as_ref().filter(|o| !o.skipped) {
            history::add(o, settings.preset.info().title);
            let _ = app.emit("history", history::load());
            if settings.open_when_done {
                if let Some(first) = o.outputs.first() {
                    let _ = app.opener().open_path(first.to_string_lossy(), None::<&str>);
                }
            }
        }
        {
            let mut q = shared.queue.lock().unwrap();
            if let Some(j) = q.iter_mut().find(|j| j.id == id) {
                j.status = status;
                j.error = error.clone();
                j.skipped = skipped;
            }
        }
        let _ = app.emit("job-finished", json!({ "id": id, "outcome": outcome, "error": error }));
        emit_queue(&app, &shared);

        let idle = !shared.queue.lock().unwrap().iter().any(|j| j.status == Status::Waiting);
        if idle {
            system::busy(false);
            // Длинная очередь кончилась, пока окно было свёрнуто, — мигнём на панели задач.
            if let Some(w) = app.get_webview_window("main") {
                if !w.is_focused().unwrap_or(true) {
                    let _ = w.request_user_attention(Some(tauri::UserAttentionType::Informational));
                }
            }
        }
    }
}

// ───────────────────────── Запуск ─────────────────────────

pub fn run() {
    let shared: State = Arc::new(Shared { settings: Mutex::new(Settings::load()), ..Default::default() });

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            // Второй запуск («Открыть с помощью», перетаскивание на ярлык) — файлы в эту очередь.
            let state = app.state::<State>();
            let paths: Vec<PathBuf> = argv.iter().skip(1).map(PathBuf::from).collect();
            add_files(app, &state, &paths);
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(shared.clone())
        .invoke_handler(tauri::generate_handler![
            bootstrap,
            advice,
            save_settings,
            enqueue,
            remove_job,
            stop,
            clear_finished,
            model_download,
            model_cancel,
            model_delete,
            open_path,
            reveal,
            history_list,
            history_remove,
            taskbar
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            let s = shared.clone();
            std::thread::Builder::new().name("stenograf-worker".into()).spawn(move || worker(handle, s))?;
            let args: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
            if !args.is_empty() {
                add_files(app.handle(), &shared, &args);
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                // Прерываем whisper, чтобы процесс не висел минуту после закрытия окна.
                let state = window.state::<State>();
                if let Some(flag) = state.current.lock().unwrap().as_ref() {
                    flag.store(true, Ordering::Relaxed);
                }
                system::busy(false);
            }
        })
        .run(tauri::generate_context!())
        .expect("не удалось запустить окно");
}

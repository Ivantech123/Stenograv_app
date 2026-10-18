// Стенограф — окно. Без сборщиков: Tauri отдаёт эту папку как есть.
"use strict";

const T = window.__TAURI__;
const $ = (s, el = document) => el.querySelector(s);
const $$ = (s, el = document) => [...el.querySelectorAll(s)];
const body = document.body;

const PRESETS = {
  fast: { title: "Быстро", speed: 5, acc: 3 },
  balanced: { title: "Баланс", speed: 4, acc: 4 },
  accurate: { title: "Точно", speed: 2, acc: 5 },
};
const STAGES = {
  model: "Скачиваю модель",
  ffmpeg: "Скачиваю декодер для этого формата",
  decoding: "Читаю запись",
  loading: "Загружаю модель в память",
  transcribing: "Распознаю речь",
  saving: "Сохраняю",
};
const GAP = 1.4; // пауза, после которой начинается новый абзац (как в writer.rs)

const app = {
  settings: null,
  hardware: null,
  models: [],
  ffmpeg: null,
  advice: null,       // подбор режима под этот ПК (advisor.rs)
  version: "",
  queue: [],          // снимок очереди от бэкенда
  jobs: new Map(),    // id → { segments, paragraphs, peaks, duration, outcome, stage, pct, dl, t0 }
  view: null,         // id задания на экране
  history: [],
  modelDl: null,      // { preset, done, total }
  follow: true,       // читалка прилипает к последней фразе
};

// ───────────────────────── Мост к Rust ─────────────────────────
const api = T ? {
  call: (cmd, args) => T.core.invoke(cmd, args),
  on: (name, fn) => T.event.listen(name, (e) => fn(e.payload)),
} : mockApi();

// ───────────────────────── Старт ─────────────────────────
const wave = new Wave($("#wave"));

(async function boot() {
  const s = await api.call("bootstrap");
  Object.assign(app, { settings: s.settings, hardware: s.hardware, advice: s.advice, models: s.models, ffmpeg: s.ffmpeg, version: s.version, history: s.history });
  applyTheme();
  bindChrome();
  bindHome();
  bindSettings();
  renderSettings();
  renderPresets();
  renderModels();
  renderArchive();
  onQueue(s.queue);

  api.on("queue", onQueue);
  api.on("job", ({ id, event }) => onJobEvent(id, event));
  api.on("job-finished", onJobFinished);
  api.on("models", (list) => { app.models = list; refreshAdvice(); });
  api.on("model-progress", (p) => { app.modelDl = p; renderModelProgress(); });
  api.on("history", (h) => { app.history = h; renderArchive(); });
  api.on("toast", (t) => toast(t.text, t.error));

  requestAnimationFrame(() => body.classList.add("ready"));
  if (T) T.window.getCurrentWindow().show();
})();

// ───────────────────────── Окно и вкладки ─────────────────────────
function bindChrome() {
  $$(".tab").forEach((b) => b.addEventListener("click", () => go(b.dataset.page)));
  // Ширина подсветки зависит от шрифта: считаем, когда Onest загрузился.
  const inks = () => { moveInk($(".tab.on"), $("#tab-ink")); moveInk($("#preset .on"), $("#preset-ink")); };
  requestAnimationFrame(inks);
  document.fonts.ready.then(inks);
  window.addEventListener("resize", () => { moveInk($(".tab.on"), $("#tab-ink")); moveInk($("#preset .on"), $("#preset-ink")); });
  if (T) {
    const w = T.window.getCurrentWindow();
    $("#win-min").onclick = () => w.minimize();
    $("#win-max").onclick = () => w.toggleMaximize();
    $("#win-close").onclick = () => w.close();
  }
  window.matchMedia("(prefers-color-scheme: light)").addEventListener("change", applyTheme);
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") closeVocab();
    if (e.ctrlKey && e.key.toLowerCase() === "o") { e.preventDefault(); pickFiles(); }
  });
}

function go(page) {
  body.dataset.page = page;
  $$(".tab").forEach((b) => b.classList.toggle("on", b.dataset.page === page));
  moveInk($(".tab.on"), $("#tab-ink"));
  if (page === "home") requestAnimationFrame(() => { wave.resize(); moveInk($("#preset .on"), $("#preset-ink")); });
  if (page === "archive") api.call("history_list").then((h) => { app.history = h; renderArchive(); });
}

function moveInk(target, ink) {
  if (!target || !ink) return;
  ink.style.width = target.offsetWidth + "px";
  ink.style.transform = `translateX(${target.offsetLeft}px)`;
}

function applyTheme() {
  const pref = app.settings?.theme || "auto";
  const light = pref === "light" || (pref === "auto" && window.matchMedia("(prefers-color-scheme: light)").matches);
  document.documentElement.dataset.theme = light ? "light" : "dark";
  requestAnimationFrame(() => wave.colors());
}

// ───────────────────────── Главная ─────────────────────────
function bindHome() {
  $("#pick-files").onclick = pickFiles;
  $("#pick-folder").onclick = pickFolder;
  $("#add-more").onclick = pickFiles;
  $("#stop").onclick = () => api.call("stop");
  $("#new-job").onclick = async () => { await api.call("clear_finished"); app.view = null; render(); };
  $("#copy-text").onclick = copyText;
  $("#reveal").onclick = () => { const o = viewJob()?.outcome; if (o?.outputs[0]) api.call("reveal", { path: o.outputs[0] }); };

  $$("#preset button").forEach((b) => b.addEventListener("click", () => choosePreset(b.dataset.preset)));
  $("#preset-hint").addEventListener("click", (e) => { if (e.target.closest("[data-auto]")) { app.settings.auto_preset = true; save(); renderPresets(); renderModels(); } });
  $("#opt-legal").addEventListener("change", (e) => { app.settings.legal = e.target.checked; save(); });
  $$("#formats input").forEach((i) => i.addEventListener("change", () => {
    const any = $$("#formats input").some((x) => x.checked);
    if (!any) { i.checked = true; toast("Нужен хотя бы один формат"); return; }
    app.settings[i.dataset.format] = i.checked; save();
  }));

  const pop = $("#vocab-pop");
  $("#vocab-btn").onclick = (e) => {
    e.stopPropagation();
    const r = $("#vocab-btn").getBoundingClientRect();
    pop.style.left = Math.min(window.innerWidth - 400, Math.max(16, r.left + r.width / 2 - 190)) + "px";
    pop.classList.toggle("open");
    if (pop.classList.contains("open")) $("#vocab").focus();
  };
  $("#vocab-done").onclick = closeVocab;
  $("#vocab-clear").onclick = () => { $("#vocab").value = ""; $("#vocab").focus(); };
  pop.addEventListener("click", (e) => e.stopPropagation());
  document.addEventListener("click", () => pop.classList.contains("open") && closeVocab());

  const scroll = $("#reader-scroll");
  scroll.addEventListener("scroll", () => {
    const atEnd = scroll.scrollHeight - scroll.scrollTop - scroll.clientHeight < 60;
    app.follow = atEnd;
    $("#to-live").classList.toggle("show", !atEnd && body.dataset.state === "run");
  });
  $("#to-live").onclick = () => { app.follow = true; scroll.scrollTop = scroll.scrollHeight; };

  bindDrop();
}

function closeVocab() {
  const pop = $("#vocab-pop");
  if (!pop.classList.contains("open")) return;
  pop.classList.remove("open");
  app.settings.vocabulary = $("#vocab").value.trim();
  save();
  renderVocabCount();
}

function renderVocabCount() {
  const n = (app.settings.vocabulary || "").split(/[,;\n]/).map((s) => s.trim()).filter(Boolean).length;
  $("#vocab-count").textContent = n ? n : "";
}

async function pickFiles() {
  if (!T) return mockEnqueue();
  const picked = await T.dialog.open({
    multiple: true,
    title: "Записи для расшифровки",
    filters: [{ name: "Звук и видео", extensions: ["wma", "mp3", "m4a", "wav", "ogg", "opus", "flac", "amr", "aac", "3gp", "mp4", "mov", "webm", "mkv", "asf", "aiff", "m4b"] }, { name: "Все файлы", extensions: ["*"] }],
  });
  if (picked) enqueue(Array.isArray(picked) ? picked : [picked]);
}

async function pickFolder() {
  if (!T) return mockEnqueue();
  const dir = await T.dialog.open({ directory: true, title: "Папка с записями" });
  if (dir) enqueue([dir]);
}

async function enqueue(paths) {
  const added = await api.call("enqueue", { paths });
  if (!added) toast("Среди выбранного нет звуковых файлов");
}

function bindDrop() {
  const on = (v) => { body.classList.toggle("dragging", v); wave.setLift(v); };
  if (T) {
    T.webview.getCurrentWebview().onDragDropEvent((e) => {
      const t = e.payload.type;
      if (t === "enter" || t === "over") on(true);
      else if (t === "leave") on(false);
      else if (t === "drop") { on(false); if (body.dataset.page !== "home") go("home"); enqueue(e.payload.paths); }
    });
  } else {
    let depth = 0;
    window.addEventListener("dragenter", (e) => { e.preventDefault(); depth++; on(true); });
    window.addEventListener("dragover", (e) => e.preventDefault());
    window.addEventListener("dragleave", () => { if (--depth <= 0) { depth = 0; on(false); } });
    window.addEventListener("drop", (e) => { e.preventDefault(); depth = 0; on(false); mockEnqueue(); });
  }
}

// ───────────────────────── Очередь и события ─────────────────────────
function jobState(id) {
  if (!app.jobs.has(id)) app.jobs.set(id, { segments: [], paragraphs: [], peaks: null, duration: 0, outcome: null, stage: null, pct: 0, dl: null, t0: 0 });
  return app.jobs.get(id);
}

function onQueue(queue) {
  app.queue = queue;
  const running = queue.find((j) => j.status === "running");
  // Экран следует за текущим заданием, пока человек не выбрал другое вручную.
  if (running && (app.view == null || !app.pinned)) app.view = running.id;
  if (!queue.some((j) => j.id === app.view)) app.view = running?.id ?? lastFinished()?.id ?? null;
  for (const id of [...app.jobs.keys()]) if (!queue.some((j) => j.id === id)) app.jobs.delete(id);
  render();
}

function lastFinished() {
  return [...app.queue].reverse().find((j) => j.status === "done" || j.status === "failed" || j.status === "skipped");
}

function onJobEvent(id, ev) {
  const j = jobState(id);
  switch (ev.kind) {
    case "stage":
      j.stage = ev.stage; j.dl = null;
      if (ev.stage === "transcribing") j.t0 = performance.now();
      break;
    case "download": j.dl = { done: ev.done, total: ev.total }; break;
    case "audio": j.peaks = ev.peaks; j.duration = ev.duration; break;
    case "progress": j.pct = Math.max(j.pct, ev.percent / 100); break;
    case "segment":
      j.segments.push(ev);
      if (j.duration) j.pct = Math.max(j.pct, Math.min(0.995, ev.end / j.duration));
      if (id === app.view) appendSegment(ev);
      break;
  }
  if (id === app.view) renderNow();
  // Подпись этапа и кольцо прогресса в очереди — не чаще раза в процент.
  const step = Math.floor(j.pct * 100);
  if (ev.kind === "stage" || step !== j.queueStep) { j.queueStep = step; renderQueue(); }
}

function onJobFinished({ id, outcome }) {
  const j = jobState(id);
  if (outcome) { j.outcome = outcome; j.paragraphs = outcome.paragraphs; j.pct = 1; if (!j.duration) j.duration = outcome.duration; }
  if (id === app.view) { renderNow(); renderTranscript(); }
}

function viewJob() {
  if (app.view == null) return null;
  const q = app.queue.find((j) => j.id === app.view);
  return q ? { ...q, ...jobState(app.view) } : null;
}

// ───────────────────────── Отрисовка главной ─────────────────────────
function render() {
  const q = app.queue;
  const running = q.some((j) => j.status === "running" || j.status === "waiting");
  const job = viewJob();
  const state = !job ? "idle" : running || job.status === "running" ? "run" : "done";
  const changed = body.dataset.state !== state;
  body.dataset.state = state;
  body.classList.toggle("has-queue", q.length > 1);
  renderQueue();
  if (changed) requestAnimationFrame(() => { wave.resize(); moveInk($("#preset .on"), $("#preset-ink")); });
  if (state === "idle") { body.classList.remove("live"); wave.setMode("idle"); setTaskbar(null); return; }
  renderNow();
  renderTranscript();
}

function renderNow() {
  const job = viewJob();
  if (!job) return;
  const done = job.status === "done";
  const failed = job.status === "failed" || job.status === "cancelled";
  body.classList.toggle("live", job.status === "running" && job.stage === "transcribing");
  $("#now-name").textContent = job.name;
  $("#now-name").title = job.path;

  let stage = STAGES[job.stage] || "В очереди";
  if (job.stage === "model") stage = `Скачиваю модель «${PRESETS[effectivePreset()].title}» — один раз`;
  if (done) stage = job.skipped ? "Уже расшифрован — пропущен" : "Готово";
  if (failed) stage = job.status === "cancelled" ? "Остановлено" : "Не получилось";
  $("#now-stage").textContent = stage;

  const meta = [];
  if (job.duration) meta.push(dur(job.duration));
  meta.push(PRESETS[effectivePreset()].title);
  if (job.dl && job.dl.total) meta.push(`${mb(job.dl.done)} из ${mb(job.dl.total)}`);
  if (job.status === "running" && job.stage === "transcribing" && job.pct > 0.04 && job.t0) {
    const spent = (performance.now() - job.t0) / 1000;
    const left = spent * (1 - job.pct) / job.pct;
    meta.push(left < 60 ? "меньше минуты" : `осталось ≈ ${dur(left)}`);
  }
  if (failed && job.error) meta.splice(0, meta.length, job.error);
  $("#now-meta").textContent = meta.join(" · ");
  $("#now-meta").classList.toggle("err", failed);

  let pct = job.pct;
  if (job.stage === "model" || job.stage === "ffmpeg") pct = job.dl && job.dl.total ? job.dl.done / job.dl.total : 0;
  if (done) pct = 1;
  $("#now-pct-num").textContent = Math.floor(pct * 100);
  $("#now-pct").style.visibility = failed || (job.status === "waiting") ? "hidden" : "visible";

  // Лента
  if (done) { wave.setPeaks(job.peaks); wave.setMode("done"); }
  else if (job.peaks && job.stage !== "model") { wave.setPeaks(job.peaks); wave.setMode("run"); wave.setProgress(job.pct); wave.setFill(null); }
  else if (job.stage === "model" || job.stage === "ffmpeg") { wave.setMode("idle"); wave.setFill(pct); }
  else { wave.setMode("scan"); wave.setFill(null); }
  $("#clock-left").textContent = clock(job.duration * (done ? 1 : job.pct));
  $("#clock-right").textContent = clock(job.duration);

  // Док
  if (!done && !failed) {
    const left = app.queue.filter((j) => j.status === "waiting").length;
    $("#run-note").innerHTML = `<svg viewBox="0 0 16 16"><rect x="3" y="7" width="10" height="7" rx="1.5"/><path d="M5.5 7V5a2.5 2.5 0 0 1 5 0v2"/></svg>` +
      (left ? `Ещё в очереди: ${left}` : "Звук не покидает компьютер");
  }
  if (done || failed) renderDoneDock(job);
  setTaskbar(done || failed ? null : pct);
}

function renderDoneDock(job) {
  const o = job.outcome;
  const stats = $("#done-stats");
  const group = $("#open-group");
  group.innerHTML = "";
  if (!o || job.status !== "done") {
    stats.innerHTML = job.status === "cancelled" ? "<span>Остановлено. Начатый текст не сохранён</span>"
      : job.status === "failed" ? `<span><em>Ошибка:</em> ${esc(job.error || "")}</span>` : "";
    $("#reveal").style.display = "none"; $("#copy-text").style.display = "none";
    return;
  }
  $("#reveal").style.display = o.outputs.length ? "" : "none";
  $("#copy-text").style.display = o.paragraphs.length ? "" : "none";
  if (o.skipped) { stats.innerHTML = "<span>Текст уже был — оставлен как есть</span>"; return; }
  const speed = o.elapsed > 0 ? o.duration / o.elapsed : 0;
  const parts = [
    `<span>${dur(o.duration)} <em>записи</em></span>`,
    `<span><em>за</em> ${dur(o.elapsed)}</span>`,
    `<span>×${speed.toFixed(1).replace(".", ",")}</span>`,
    `<span>${o.words.toLocaleString("ru")} <em>${plural(o.words, "слово", "слова", "слов")}</em></span>`,
  ];
  const cleaned = o.phantoms_removed + o.repeats_removed;
  if (cleaned) parts.push(`<span title="Фантомные титры и повторы, которые Whisper иногда выдумывает на тишине"><em>убрано мусора:</em> ${cleaned}</span>`);
  stats.innerHTML = parts.join("");
  o.outputs.forEach((p, i) => {
    const ext = p.split(".").pop().toUpperCase();
    const b = document.createElement("button");
    b.className = "btn " + (i === 0 ? "primary" : "ghost");
    b.textContent = i === 0 ? `Открыть ${ext}` : ext;
    b.title = p;
    b.onclick = () => api.call("open_path", { path: p });
    group.appendChild(b);
  });
}

function renderTranscript() {
  const job = viewJob();
  const box = $("#transcript");
  if (!job) { box.innerHTML = ""; return; }
  const paras = job.outcome ? job.paragraphs : group(job.segments);
  if (!paras.length) {
    const msg = job.status === "done" ? "Речь не обнаружена" : job.status === "running" ? "" : job.status === "waiting" ? "Ждёт своей очереди" : "";
    box.innerHTML = msg ? `<div class="reader-empty">${msg}</div>` : "";
    return;
  }
  box.innerHTML = paras.map((p) => `<div class="para"><time>${clock(p.start)}</time><p>${esc(p.text)}</p></div>`).join("");
  if (app.follow) $("#reader-scroll").scrollTop = 1e9;
}

/** Живая фраза: дописываем в последний абзац или начинаем новый — без перерисовки всего текста. */
function appendSegment(seg) {
  const box = $("#transcript");
  const empty = box.querySelector(".reader-empty");
  if (empty) empty.remove();
  const job = jobState(app.view);
  const prev = job.segments[job.segments.length - 2];
  let p = box.lastElementChild?.querySelector("p");
  if (!p || !prev || seg.start - prev.end > GAP) {
    const div = document.createElement("div");
    div.className = "para";
    div.innerHTML = `<time>${clock(seg.start)}</time><p></p>`;
    box.appendChild(div);
    p = div.querySelector("p");
  }
  const span = document.createElement("span");
  span.className = "fresh";
  span.textContent = (p.childNodes.length ? " " : "") + seg.text.trim();
  p.appendChild(span);
  requestAnimationFrame(() => requestAnimationFrame(() => span.classList.remove("fresh")));
  if (app.follow) $("#reader-scroll").scrollTop = 1e9;
}

function group(segments) {
  const out = [];
  let prevEnd = -1e9;
  for (const s of segments) {
    const text = s.text.trim();
    if (!text) continue;
    if (!out.length || s.start - prevEnd > GAP) out.push({ start: s.start, text });
    else out[out.length - 1].text += " " + text;
    prevEnd = s.end;
  }
  return out;
}

function renderQueue() {
  const list = $("#queue-list");
  const q = app.queue;
  $("#queue-count").textContent = q.length ? `${q.filter((j) => j.status === "done").length} / ${q.length}` : "";
  list.innerHTML = "";
  for (const j of q) {
    const st = jobState(j.id);
    const li = document.createElement("li");
    li.className = `q ${j.status}` + (j.id === app.view ? " sel" : "");
    const sub = {
      waiting: "в очереди",
      running: STAGES[st.stage] ? STAGES[st.stage].toLowerCase() : "начинаю",
      done: j.skipped ? "уже был, пропущен" : st.outcome ? `${dur(st.outcome.duration)} · ${st.outcome.words} сл.` : "готово",
      failed: j.error || "ошибка",
      cancelled: "остановлено",
    }[j.status];
    li.innerHTML = `${icon(j, st)}<div style="min-width:0"><div class="q-name" title="${esc(j.path)}">${esc(j.name)}</div><div class="q-sub">${esc(sub || "")}</div></div>` +
      (j.status === "running" ? "<span></span>" : `<button class="q-x" title="Убрать"><svg viewBox="0 0 10 10"><path d="M2 2l6 6M8 2l-6 6"/></svg></button>`);
    li.onclick = () => { app.view = j.id; app.pinned = j.status !== "running"; app.follow = true; render(); };
    const x = li.querySelector(".q-x");
    if (x) x.onclick = (e) => { e.stopPropagation(); api.call("remove_job", { id: j.id }); };
    list.appendChild(li);
  }
}

function icon(j, st) {
  if (j.status === "running") {
    const c = 2 * Math.PI * 8;
    const p = st.stage === "transcribing" ? st.pct : 0;
    return `<svg class="ring ${p ? "" : "spin"}" viewBox="0 0 20 20"><circle class="bg" cx="10" cy="10" r="8"/><circle class="fg" cx="10" cy="10" r="8" stroke-dasharray="${c}" stroke-dashoffset="${p ? c * (1 - p) : c * 0.72}"/></svg>`;
  }
  const paths = {
    waiting: `<circle cx="8" cy="8" r="5.5" stroke-dasharray="2 2.6"/>`,
    done: `<path d="M3.5 8.5l3 3 6-7"/>`,
    failed: `<path d="M8 4.5v4.2M8 11.4v.1"/><circle cx="8" cy="8" r="6"/>`,
    cancelled: `<path d="M4 8h8"/>`,
  };
  return `<span class="state-ico"><svg viewBox="0 0 16 16">${paths[j.status] || ""}</svg></span>`;
}

async function copyText() {
  const job = viewJob();
  const paras = job?.outcome?.paragraphs || [];
  const text = paras.map((p) => p.text).join("\n\n");
  try { await navigator.clipboard.writeText(text); toast("Текст скопирован"); } catch { toast("Не удалось скопировать", true); }
}

function setTaskbar(p) {
  if (!T) return;
  api.call("taskbar", { progress: p == null ? null : Math.round(p * 100) }).catch(() => {});
}

/** Режим, которым будет считаться следующий файл. */
function effectivePreset() {
  return app.settings.auto_preset && app.advice ? app.advice.preset : app.settings.preset;
}

function choosePreset(preset) {
  app.settings.preset = preset;
  app.settings.auto_preset = false;
  save(); renderPresets(); renderModels();
}

async function refreshAdvice() {
  if (T) app.advice = await api.call("advice");
  renderPresets(); renderModels();
}

/** «≈ 25 мин на час записи» из оценки скорости. */
function perHour(preset) {
  const e = app.advice?.estimates.find((x) => x.preset === preset);
  if (!e || !e.speed) return "";
  const min = Math.round(60 / e.speed);
  return min >= 60 ? `≈ ${Math.floor(min / 60)} ч ${String(min % 60).padStart(2, "0")} мин на час записи` : `≈ ${Math.max(1, min)} мин на час записи`;
}

function renderPresets() {
  const cur = effectivePreset();
  $$("#preset button").forEach((b) => { b.classList.toggle("on", b.dataset.preset === cur); b.setAttribute("aria-checked", b.dataset.preset === cur); });
  requestAnimationFrame(() => moveInk($("#preset .on"), $("#preset-ink")));
  const m = app.models.find((x) => x.preset === cur);
  if (m) {
    const auto = app.settings.auto_preset ? `<b>подобран под этот ПК</b>` : `<a class="link" data-auto>подобрать под ПК</a>`;
    const ready = m.installed ? "" : ` · скачается ${mb(m.size)}`;
    $("#preset-hint").innerHTML = `${auto}<br>${perHour(cur)}${ready}`;
    $("#preset-hint").title = (app.advice?.reasons || []).join("\n");
  }
  $("#opt-legal").checked = !!app.settings.legal;
  $$("#formats input").forEach((i) => (i.checked = !!app.settings[i.dataset.format]));
  $("#vocab").value = app.settings.vocabulary || "";
  renderVocabCount();
}

// ───────────────────────── Архив ─────────────────────────
function renderArchive() {
  const box = $("#archive");
  const q = ($("#archive-search").value || "").trim().toLowerCase();
  const items = app.history.filter((h) => !q || h.name.toLowerCase().includes(q) || (h.preview || "").toLowerCase().includes(q));
  $("#archive-empty").classList.toggle("show", !items.length);
  $("#archive-empty p").textContent = app.history.length ? "Ничего не нашлось" : "Здесь появятся готовые расшифровки";
  let html = "", lastDay = "";
  for (const h of items) {
    const day = dayLabel(h.finished_at);
    if (day !== lastDay) { html += `<div class="day">${day}</div>`; lastDay = day; }
    const time = new Date(h.finished_at).toLocaleTimeString("ru", { hour: "2-digit", minute: "2-digit" });
    html += `<div class="arow ${h.exists ? "" : "missing"}" data-id="${h.id}">
      <div style="min-width:0"><div class="arow-name" title="${esc(h.source)}">${esc(h.name)}</div><div class="arow-preview">${esc(h.preview || "")}</div></div>
      <div class="arow-actions">
        ${h.exists ? `<button class="btn ghost small" data-act="open">Открыть</button><button class="btn ghost small" data-act="reveal">Папка</button>` : `<span class="arow-meta">файл удалён</span>`}
        <button class="btn ghost small icon" data-act="forget" title="Убрать из архива"><svg viewBox="0 0 16 16"><path d="M4 4l8 8M12 4l-8 8"/></svg></button>
      </div>
      <div class="arow-meta">${dur(h.duration)} · ${h.words} сл.<br>${time}</div>
    </div>`;
  }
  box.innerHTML = html;
  $$(".arow [data-act]", box).forEach((b) => b.addEventListener("click", () => {
    const h = app.history.find((x) => x.id === b.closest(".arow").dataset.id);
    if (!h) return;
    if (b.dataset.act === "open") api.call("open_path", { path: h.outputs[0] });
    if (b.dataset.act === "reveal") api.call("reveal", { path: h.outputs[0] });
    if (b.dataset.act === "forget") api.call("history_remove", { id: h.id });
  }));
}
$("#archive-search").addEventListener("input", renderArchive);

// ───────────────────────── Модели ─────────────────────────
function renderModels() {
  const box = $("#models");
  box.innerHTML = app.models.map((m) => {
    const p = PRESETS[m.preset];
    const dl = app.modelDl && app.modelDl.preset === m.preset;
    const meter = (n) => `<div class="meter">${[1, 2, 3, 4, 5].map((i) => `<i class="${i <= n ? "on" : ""}"></i>`).join("")}</div>`;
    let status, action;
    if (dl) { status = `${mb(app.modelDl.done)} из ${mb(app.modelDl.total)}`; action = `<button class="btn ghost small" data-act="cancel">Отменить</button>`; }
    else if (m.installed) { status = `<b>Скачана</b> · ${mb(m.size)}`; action = `<button class="btn ghost small" data-act="delete">Удалить</button>`; }
    else if (m.partial) { status = `Скачано ${mb(m.partial)} из ${mb(m.size)}`; action = `<button class="btn primary small" data-act="download">Докачать</button>`; }
    else { status = mb(m.size); action = `<button class="btn primary small" data-act="download">Скачать</button>`; }
    const w = dl && app.modelDl.total ? (app.modelDl.done / app.modelDl.total) * 100 : 0;
    return `<div class="model ${m.preset === effectivePreset() ? "current" : ""}" data-preset="${m.preset}">
      <div><h3>${p.title}</h3><div class="mname">${esc(m.model)}</div></div>
      <p>${esc(m.hint)}<br><span class="est">${perHour(m.preset)}${app.advice?.estimates.find((e) => e.preset === m.preset)?.measured ? " · замер" : ""}</span></p>
      <div class="meters"><span>Скорость</span>${meter(p.speed)}<span>Точность</span>${meter(p.acc)}</div>
      <div class="model-foot"><span class="model-size">${status}</span>${action}</div>
      <div class="model-bar" style="width:${w}%"></div>
    </div>`;
  }).join("");
  $$(".model", box).forEach((card) => {
    card.addEventListener("click", (e) => {
      const act = e.target.closest("[data-act]")?.dataset.act;
      const preset = card.dataset.preset;
      if (act === "download") api.call("model_download", { preset });
      else if (act === "cancel") api.call("model_cancel");
      else if (act === "delete") api.call("model_delete", { preset });
      else choosePreset(preset);
    });
  });

  const hw = app.hardware, adv = app.advice;
  const gb = (b) => (b / 1073741824).toFixed(1).replace(".", ",") + " ГБ";
  const cores = hw.efficiency_cores ? `${hw.performance_cores} P + ${hw.efficiency_cores} E · ${hw.logical_cores} потоков` : `${hw.physical_cores} ядер · ${hw.logical_cores} потоков`;
  const gpu = (hw.gpus || []).map((g) => g.name + (g.vram ? ` · ${gb(g.vram)}` : "")).join(", ") || "не найдена";
  const facts = [
    ["Процессор", hw.cpu],
    ["Ядра", cores],
    ["Инструкции", hw.avx512 ? "AVX2 · AVX-512" : hw.avx2 ? "AVX2 · FMA" : "без AVX2"],
    ["Память", hw.ram_total ? `${gb(hw.ram_available)} свободно из ${gb(hw.ram_total)}` : "—"],
    ["Видеокарта", gpu],
    ["Считает", adv ? (adv.gpu ? "видеокарта (Vulkan)" : `процессор, ${adv.threads} потоков`) : "—"],
    ["NPU", hw.npu || "нет"],
    ["Питание", hw.power?.on_battery ? `батарея ${hw.power.battery_percent ?? "?"}%` : "от сети"],
    ["Декодер WMA", app.ffmpeg ? "ffmpeg найден" : "скачается при первой WMA"],
    ["Движок", `whisper.cpp ${hw.whisper_cpp} · Silero VAD`],
  ];
  if (adv?.blocker) facts.unshift(["Внимание", adv.blocker]);
  $("#facts").innerHTML = facts.map(([k, v]) => `<div class="fact"><span>${k}</span><b title="${esc(v)}">${esc(v)}</b></div>`).join("") +
    (adv ? `<div class="why"><span>Почему «${PRESETS[adv.preset].title}»</span>${adv.reasons.map((r) => `<p>${esc(r)}</p>`).join("")}</div>` : "");
}

function renderModelProgress() {
  const d = app.modelDl;
  if (!d) return renderModels();
  const card = $(`.model[data-preset="${d.preset}"]`);
  if (!card || !card.querySelector("[data-act=cancel]")) return renderModels();
  card.querySelector(".model-size").textContent = `${mb(d.done)} из ${mb(d.total)}`;
  card.querySelector(".model-bar").style.width = (d.total ? (d.done / d.total) * 100 : 0) + "%";
}

// ───────────────────────── Параметры ─────────────────────────
function bindSettings() {
  $("#set-language").addEventListener("change", (e) => { app.settings.language = e.target.value; save(); });
  $("#set-threads").addEventListener("input", (e) => { app.settings.threads = +e.target.value; renderThreads(); });
  $("#set-threads").addEventListener("change", () => { save(); setTimeout(refreshAdvice, 300); });
  $("#set-gpu").addEventListener("change", (e) => { app.settings.gpu = e.target.checked; save(); setTimeout(refreshAdvice, 300); });
  for (const [id, key] of [["#set-header", "header"], ["#set-timestamps", "timestamps"], ["#set-open", "open_when_done"]]) {
    $(id).addEventListener("change", (e) => { app.settings[key] = e.target.checked; save(); });
  }
  seg("#set-exists", (v) => { app.settings.on_exists = v; save(); });
  seg("#set-theme", (v) => { app.settings.theme = v; save(); applyTheme(); });
  seg("#set-outdir", async (v) => {
    if (v === "near") app.settings.out_dir = null;
    else if (T) {
      const dir = await T.dialog.open({ directory: true, title: "Куда сохранять тексты" });
      if (!dir) return renderSettings();
      app.settings.out_dir = dir;
    }
    save(); renderSettings();
  });
}

function seg(sel, fn) {
  $$(`${sel} button`).forEach((b) => b.addEventListener("click", () => { $$(`${sel} button`).forEach((x) => x.classList.toggle("on", x === b)); fn(b.dataset.v); }));
}

function renderSettings() {
  const s = app.settings, hw = app.hardware;
  $("#set-language").value = s.language;
  $("#set-threads").max = hw.logical_cores;
  $("#set-threads").value = s.threads;
  renderThreads();
  $("#set-gpu").checked = s.gpu && hw.gpu_build;
  $("#row-gpu").classList.toggle("disabled", !hw.gpu_build);
  $("#gpu-hint").textContent = hw.gpu_build ? "Radeon, GeForce или Intel Arc через Vulkan — в разы быстрее процессора" : "Эта сборка считает только на процессоре";
  $("#set-header").checked = s.header;
  $("#set-timestamps").checked = s.timestamps;
  $("#set-open").checked = s.open_when_done;
  setSeg("#set-exists", s.on_exists);
  setSeg("#set-theme", s.theme);
  setSeg("#set-outdir", s.out_dir ? "pick" : "near");
  $("#outdir-hint").textContent = s.out_dir || "Рядом с записью, то же имя";
  $("#colophon").textContent = `Стенограф ${app.version} · whisper.cpp ${hw.whisper_cpp} · звук не покидает компьютер`;
}

function renderThreads() {
  const v = app.settings.threads;
  $("#threads-out").textContent = v ? v : "авто";
  $("#threads-hint").textContent = v ? `Вручную. Физических ядер: ${app.hardware.physical_cores}` : `По числу физических ядер — ${app.hardware.physical_cores}`;
}

function setSeg(sel, v) { $$(`${sel} button`).forEach((b) => b.classList.toggle("on", b.dataset.v === v)); }

let saveTimer;
function save() {
  clearTimeout(saveTimer);
  saveTimer = setTimeout(() => api.call("save_settings", { settings: app.settings }).catch((e) => toast(String(e), true)), 150);
}

// ───────────────────────── Мелочи ─────────────────────────
let toastTimer;
function toast(text, error = false) {
  const t = $("#toast");
  t.textContent = text;
  t.classList.toggle("error", !!error);
  t.classList.add("show");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => t.classList.remove("show"), error ? 6000 : 2600);
}

function esc(s) { return String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c])); }
function clock(sec) {
  sec = Math.max(0, Math.round(sec || 0));
  const h = Math.floor(sec / 3600), m = Math.floor(sec / 60) % 60, s = sec % 60;
  return (h ? h + ":" + String(m).padStart(2, "0") : String(m).padStart(2, "0")) + ":" + String(s).padStart(2, "0");
}
function dur(sec) {
  sec = Math.max(0, Math.round(sec || 0));
  const h = Math.floor(sec / 3600), m = Math.floor(sec / 60) % 60, s = sec % 60;
  if (h) return `${h} ч ${String(m).padStart(2, "0")} мин`;
  if (m) return `${m} мин ${String(s).padStart(2, "0")} с`;
  return `${s} с`;
}
function mb(bytes) {
  const v = (bytes || 0) / 1048576;
  return v >= 1024 ? (v / 1024).toFixed(2).replace(".", ",") + " ГБ" : Math.round(v) + " МБ";
}
function plural(n, one, few, many) {
  const a = n % 10, b = n % 100;
  return a === 1 && b !== 11 ? one : a >= 2 && a <= 4 && (b < 10 || b >= 20) ? few : many;
}
function dayLabel(ms) {
  const d = new Date(ms), today = new Date();
  const diff = Math.round((new Date(today.toDateString()) - new Date(d.toDateString())) / 864e5);
  if (diff === 0) return "Сегодня";
  if (diff === 1) return "Вчера";
  return d.toLocaleDateString("ru", { day: "numeric", month: "long", year: d.getFullYear() === today.getFullYear() ? undefined : "numeric" });
}

// ───────────────────────── Макет без Rust (для вёрстки) ─────────────────────────
function mockApi() {
  const flags = new Set(location.hash.slice(1).split("&").filter(Boolean));
  const listeners = {};
  const emit = (n, p) => (listeners[n] || []).forEach((f) => f(p));
  const settings = { auto_preset: true, preset: "balanced", language: "ru", threads: 0, gpu: false, legal: true, vocabulary: "Иванов, ООО «Вектор», А40-12345/2026", timestamps: true, header: true, txt: true, docx: true, srt: false, out_dir: null, on_exists: "rename", open_when_done: false, theme: flags.has("light") ? "light" : flags.has("dark") ? "dark" : "auto" };
  const models = [
    { preset: "fast", title: "Быстро", hint: "Черновик. Та же модель, что «Баланс», но без перебора вариантов", model: "large-v3-turbo · q8 · greedy", size: 874188075, installed: true, partial: 0 },
    { preset: "balanced", title: "Баланс", hint: "Лучшее соотношение для русской диктовки", model: "large-v3-turbo · q8", size: 874188075, installed: true, partial: 0 },
    { preset: "accurate", title: "Точно", hint: "Полная large-v3: тихая и неразборчивая речь. Нужна видеокарта — на процессоре в 6 раз дольше", model: "large-v3 · q5", size: 1081140203, installed: false, partial: 312e6 },
  ];
  const text = [
    [0.4, 6.8, "Диктовка по делу номер А40-12345/2026."],
    [7.1, 15.9, "Истец, общество с ограниченной ответственностью «Вектор», обратился в Арбитражный суд города Москвы с исковым заявлением о взыскании неустойки по договору поставки № 14."],
    [19.2, 28.4, "Ответчик заявил ходатайство об отложении судебного заседания и о применении статьи 333 Гражданского кодекса Российской Федерации."],
    [32.5, 38.0, "Суд удалился в совещательную комнату и огласил резолютивную часть решения."],
    [38.3, 40.1, "Требования удовлетворены частично."],
    [40.6, 44.9, "Следующее заседание назначено на 12 ноября."],
  ];
  const peaks = Array.from({ length: 360 }, (_, i) => {
    const t = i / 360 * 46;
    const speech = text.some(([a, b]) => t >= a && t <= b) ? 1 : 0.06;
    return Math.min(1, speech * (0.45 + 0.4 * Math.abs(Math.sin(i * 0.37) * Math.cos(i * 0.11)) + 0.15 * Math.random()));
  });
  const names = ["Заседание 14.10 — Вектор.wma", "Консультация Петров.m4a", "Диктовка иск.mp3"];
  let queue = [];
  if (flags.has("run") || flags.has("done") || flags.has("queue")) {
    const n = flags.has("queue") ? 3 : 1;
    queue = names.slice(0, n).map((name, i) => ({ id: i + 1, name, path: "D:\\Диктофон\\" + name, status: i === 0 ? (flags.has("done") ? "done" : "running") : "waiting", error: null, skipped: false }));
  }
  const history = [
    { id: "a", name: "Заседание 14.10 — Вектор.wma", source: "D:\\Диктофон\\Заседание 14.10 — Вектор.wma", outputs: ["D:\\Диктофон\\Заседание 14.10 — Вектор.txt"], duration: 2843, words: 4120, finished_at: Date.now() - 36e5, preview: "Истец, общество с ограниченной ответственностью «Вектор», обратился в Арбитражный суд…", exists: true },
    { id: "b", name: "Консультация Петров.m4a", source: "D:\\Диктофон\\Консультация Петров.m4a", outputs: ["D:\\Диктофон\\Консультация Петров.txt"], duration: 1260, words: 1904, finished_at: Date.now() - 864e5 - 5e6, preview: "Добрый день. Ситуация такая: в марте мы заключили договор аренды…", exists: true },
    { id: "c", name: "Голосовая заметка 7.amr", source: "D:\\Диктофон\\Голосовая заметка 7.amr", outputs: ["D:\\Диктофон\\Голосовая заметка 7.txt"], duration: 94, words: 152, finished_at: Date.now() - 6 * 864e5, preview: "Напомнить про госпошлину и доверенность до пятницы.", exists: false },
  ];
  setTimeout(() => {
    if (!queue.length) return;
    const id = 1;
    emit("job", { id, event: { kind: "audio", duration: 46, peaks } });
    emit("job", { id, event: { kind: "stage", stage: "transcribing" } });
    const upto = flags.has("done") ? text.length : 3;
    text.slice(0, upto).forEach(([start, end, t]) => emit("job", { id, event: { kind: "segment", start, end, text: t } }));
    emit("job", { id, event: { kind: "progress", percent: flags.has("done") ? 100 : 58 } });
    if (flags.has("done")) {
      emit("job-finished", { id, outcome: { outputs: ["D:\\Диктофон\\Заседание 14.10 — Вектор.txt", "D:\\Диктофон\\Заседание 14.10 — Вектор.docx"], duration: 46, elapsed: 7.4, words: 68, phantoms_removed: 1, repeats_removed: 0, skipped: false, paragraphs: group(text.map(([start, end, text]) => ({ start, end, text }))) } });
    }
  }, 200);
  if (flags.has("archive")) setTimeout(() => go("archive"), 50);
  if (flags.has("models")) setTimeout(() => go("models"), 50);
  if (flags.has("settings")) setTimeout(() => go("settings"), 50);
  if (flags.has("dl")) setTimeout(() => emit("model-progress", { preset: "accurate", done: 640e6, total: 1081140203 }), 300);
  if (flags.has("drag")) setTimeout(() => { body.classList.add("dragging"); wave.setLift(true); }, 300);
  return {
    call: async (cmd) => {
      if (cmd === "bootstrap") return {
        settings, models, queue, history, ffmpeg: null, version: "2.0.0",
        hardware: { cpu: "12th Gen Intel(R) Core(TM) i5-12400F", physical_cores: 6, logical_cores: 12, performance_cores: 6, efficiency_cores: 0, avx2: true, avx512: false, ram_total: 34e9, ram_available: 19e9, gpus: [{ name: "NVIDIA GeForce RTX 4060", vram: 8589934592, integrated: false }], npu: null, power: { on_battery: false, battery_percent: null }, gpu_build: true, whisper_cpp: "1.8.3" },
        advice: { preset: "balanced", threads: 6, gpu: true, blocker: null, reasons: ["NVIDIA GeForce RTX 4060: час записи ≈ 3 мин (×18,0)"], estimates: [{ preset: "accurate", speed: 7, measured: false }, { preset: "balanced", speed: 18, measured: true }, { preset: "fast", speed: 30, measured: false }] },
      };
      if (cmd === "history_list") return history;
      return null;
    },
    on: (n, f) => { (listeners[n] ||= []).push(f); },
  };
}

function mockEnqueue() { toast("В макете файлы не добавляются"); }

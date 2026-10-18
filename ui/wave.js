// Звуковая лента: дышит в покое, поднимается под перетаскиваемым файлом,
// во время работы показывает огибающую записи и бегущую каретку.
"use strict";

class Wave {
  constructor(canvas) {
    this.canvas = canvas;
    this.ctx = canvas.getContext("2d");
    this.mode = "idle";      // idle | scan | run | done
    this.peaks = null;
    this.progress = 0;       // 0..1 — сколько записи уже прочитано
    this.shown = 0;          // плавно догоняет progress
    this.fill = null;        // 0..1 — заливка при скачивании модели
    this.lift = 0;           // 0..1 — над окном тащат файл
    this.liftTarget = 0;
    this.cur = [];
    // #still — макет для скриншотов: кадр сразу в конечном состоянии.
    this.still = /(^#|&)still(&|$)/.test(location.hash);
    if (this.still) document.body.classList.add("still");
    this.colors();
    new ResizeObserver(() => this.resize()).observe(canvas);
    this.resize();
    const loop = (t) => { this.frame(t); requestAnimationFrame(loop); };
    requestAnimationFrame(loop);
  }

  colors() {
    const css = getComputedStyle(document.documentElement);
    this.ink = css.getPropertyValue("--ink").trim() || "#fff";
    this.dim = css.getPropertyValue("--ink-dim").trim() || "rgba(255,255,255,.15)";
    this.faint = css.getPropertyValue("--faint").trim() || "#444";
  }

  resize() {
    const dpr = window.devicePixelRatio || 1;
    const r = this.canvas.getBoundingClientRect();
    this.w = r.width; this.h = r.height;
    this.canvas.width = Math.max(1, Math.round(r.width * dpr));
    this.canvas.height = Math.max(1, Math.round(r.height * dpr));
    this.ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    const n = Math.max(24, Math.floor(r.width / 6));
    if (n !== this.cur.length) this.cur = Array.from({ length: n }, (_, i) => this.cur[Math.floor(i * this.cur.length / n)] || 0);
  }

  setMode(mode) { this.mode = mode; if (mode === "idle") { this.peaks = null; this.progress = 0; this.shown = 0; this.fill = null; } }
  setPeaks(peaks) { this.peaks = peaks && peaks.length ? peaks : null; }
  setProgress(p) { this.progress = Math.max(0, Math.min(1, p)); }
  setFill(f) { this.fill = f == null ? null : Math.max(0, Math.min(1, f)); }
  setLift(on) { this.liftTarget = on ? 1 : 0; }

  sample(x) {
    // Огибающая с линейной интерполяцией: лента не «ступенчатая» на широком окне.
    const p = this.peaks, f = x * (p.length - 1), i = Math.floor(f), k = f - i;
    return p[i] * (1 - k) + (p[Math.min(i + 1, p.length - 1)] || 0) * k;
  }

  frame(t) {
    const { ctx, w, h } = this;
    if (!w || !h) return;
    ctx.clearRect(0, 0, w, h);
    const n = this.cur.length;
    const step = w / n;
    const bar = Math.max(1.6, step * 0.42);
    this.lift = this.still ? this.liftTarget : this.lift + (this.liftTarget - this.lift) * 0.08;
    this.shown = this.still ? this.progress : this.shown + (this.progress - this.shown) * 0.08;
    const head = this.shown;
    const mid = h / 2;
    const s = t * 0.001;

    for (let i = 0; i < n; i++) {
      const x = i / (n - 1);
      const edge = Math.pow(Math.sin(Math.PI * Math.min(1, Math.max(0, x))), 0.55);
      let target;
      if (this.peaks && (this.mode === "run" || this.mode === "done")) {
        const v = this.sample(x);
        // Около каретки лента «звучит» — лёгкая дрожь только там, где идёт чтение.
        const near = this.mode === "run" ? Math.exp(-Math.pow((x - head) * 28, 2)) : 0;
        target = 0.04 + v * 0.9 + near * 0.12 * Math.sin(s * 14 + i);
      } else {
        // Покой: «голос» из трёх медленных волн — живой, но не суетливый.
        const a = Math.sin(s * 0.9 + i * 0.16) * 0.5 + 0.5;
        const b = Math.sin(s * 0.55 - i * 0.045 + 1.3) * 0.5 + 0.5;
        const c = Math.sin(s * 1.7 + i * 0.41 + 0.6) * 0.5 + 0.5;
        target = (0.05 + 0.2 * a * b + 0.06 * c) * Math.pow(edge, 1.6);
        if (this.mode === "scan") {
          // Идёт подготовка: по ленте бежит волна, но огибающей ещё нет.
          const pos = (s * 0.35) % 1.4 - 0.2;
          target += 0.32 * Math.exp(-Math.pow((x - pos) * 9, 2)) * edge;
        }
        target += this.lift * edge * (0.22 + 0.16 * Math.sin(s * 3.2 + i * 0.32) + 0.1 * Math.sin(s * 5.1 - i * 0.55));
      }
      this.cur[i] = this.still ? target : this.cur[i] + (target - this.cur[i]) * 0.14;
      const amp = Math.max(0.012, Math.min(1, this.cur[i]));
      const half = Math.max(1, amp * h * 0.48);
      const cx = i * step + step / 2;

      let color = this.dim;
      if (this.mode === "done") color = this.ink;
      else if (this.mode === "run" && this.peaks) color = x <= head ? this.ink : this.dim;
      else if (this.fill != null) color = x <= this.fill ? this.ink : this.dim;
      else if (this.lift > 0.05) color = this.ink;
      ctx.globalAlpha = this.mode === "idle" && this.fill == null ? 0.8 + 0.2 * this.lift : 1;
      ctx.fillStyle = color;
      roundBar(ctx, cx - bar / 2, mid - half, bar, half * 2, bar / 2);
    }
    ctx.globalAlpha = 1;

    if (this.mode === "run" && this.peaks) {
      const x = head * w;
      ctx.fillStyle = this.ink;
      ctx.fillRect(Math.round(x), 0, 1, h);
      ctx.beginPath();
      ctx.arc(x + 0.5, 3, 3, 0, Math.PI * 2);
      ctx.fill();
    }
  }
}

function roundBar(ctx, x, y, w, h, r) {
  r = Math.min(r, h / 2, w / 2);
  ctx.beginPath();
  ctx.moveTo(x + r, y);
  ctx.arcTo(x + w, y, x + w, y + h, r);
  ctx.arcTo(x + w, y + h, x, y + h, r);
  ctx.arcTo(x, y + h, x, y, r);
  ctx.arcTo(x, y, x + w, y, r);
  ctx.closePath();
  ctx.fill();
}

window.Wave = Wave;

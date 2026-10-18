//! Выбор режима под конкретный компьютер.
//!
//! 1. Оцениваем скорость каждого режима (× к реальному времени) по ядрам и SIMD.
//!    Как только на этом ПК прошла хоть одна расшифровка — берём замер, а не оценку.
//! 2. Отсекаем режимы, которым не хватит свободной памяти или места на диске.
//! 3. Берём самый точный режим, который успевает: «Баланс» — от ×1, иначе «Быстро».
//!    Медленнее реального времени часовое заседание считалось бы больше часа.
//!    «Точно» (полная large-v3) сам не выбираем: на тестовой диктовке она ошиблась
//!    там, где turbo был прав («по ставке» вместо «поставки»), — только вручную.
//! 4. На батарее с зарядом меньше половины — на ступень легче.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::hardware::HardwareInfo;
use crate::models::Preset;
use crate::paths;

/// Эталон для оценок: 6 производительных ядер с AVX2 (Core i5-12400F),
/// сборка whisper.cpp с AVX2/FMA, Silero VAD. Замер 18.10.2026 на 46 с диктовки:
/// turbo q8 beam 5 — ×1,7; large-v3 q5 beam 5 — ×0,28.
const REF_POWER: f64 = 6.0;

fn reference_speed(preset: Preset) -> f64 {
    match preset {
        Preset::Fast => 2.1,
        Preset::Balanced => 1.8,
        Preset::Accurate => 0.3,
    }
}

/// Видеокарта через Vulkan. Замер на GeForce RTX 4060 8 ГБ (прогретые шейдеры):
/// «Быстро» ×64, «Баланс» ×35, «Точно» ×15. Для оценки берём с запасом на карты
/// слабее; встроенная графика — ещё вчетверо медленнее. Первый же замер уточнит.
fn gpu_speed(preset: Preset, integrated: bool) -> f64 {
    let discrete = match preset {
        Preset::Fast => 30.0,
        Preset::Balanced => 18.0,
        Preset::Accurate => 7.0,
    };
    if integrated { discrete / 4.0 } else { discrete }
}

/// Рабочая память на модель, буферы и beam search, байт.
fn ram_need(preset: Preset) -> u64 {
    const MB: u64 = 1 << 20;
    match preset {
        Preset::Fast => 1100 * MB,
        Preset::Balanced => 1500 * MB,
        Preset::Accurate => 2400 * MB,
    }
}

fn min_speed(preset: Preset) -> f64 {
    match preset {
        Preset::Accurate => f64::INFINITY,
        Preset::Balanced => 1.0,
        Preset::Fast => 0.0,
    }
}

const ORDER: [Preset; 3] = [Preset::Accurate, Preset::Balanced, Preset::Fast];

#[derive(Debug, Clone, Serialize)]
pub struct Estimate {
    pub preset: Preset,
    /// Во сколько раз быстрее реального времени.
    pub speed: f64,
    /// true — из замеров на этом ПК, false — оценка по железу.
    pub measured: bool,
    pub fits_ram: bool,
    pub fits_disk: bool,
    pub installed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Advice {
    pub preset: Preset,
    pub threads: usize,
    pub gpu: bool,
    pub estimates: Vec<Estimate>,
    /// Почему выбран этот режим — показывается человеку как есть.
    pub reasons: Vec<String>,
    /// Что мешает работать вовсе (нет AVX2) — показать крупно.
    pub blocker: Option<String>,
}

impl Advice {
    pub fn estimate(&self, preset: Preset) -> Option<&Estimate> {
        self.estimates.iter().find(|e| e.preset == preset)
    }

    /// Сколько секунд займёт запись длиной `audio` в выбранном режиме.
    pub fn eta(&self, preset: Preset, audio: f64) -> Option<f64> {
        self.estimate(preset).filter(|e| e.speed > 0.0).map(|e| audio / e.speed)
    }
}

/// Вычислительная мощность в «эталонных ядрах».
fn cpu_power(hw: &HardwareInfo, threads: usize) -> f64 {
    let p = hw.performance_cores.max(1) as f64;
    let e = hw.efficiency_cores as f64;
    let threads = threads.max(1) as f64;
    // Потоки сверх P-ядер садятся на E-ядра, которые примерно вдвое слабее.
    let cores = if threads <= p { threads } else { p + (threads - p).min(e) * 0.45 };
    // Сборка — AVX2: AVX-512 сверх этого ничего не даёт, без AVX2 не запустится.
    let simd = if hw.avx2 && hw.fma { 1.0 } else { 0.0 };
    // Масштабирование не линейное: синхронизация потоков съедает часть выигрыша.
    REF_POWER * (cores / REF_POWER).powf(0.85) * simd
}

pub fn advise(hw: &HardwareInfo, gpu_wanted: bool, threads_setting: usize) -> Advice {
    advise_with(hw, gpu_wanted, threads_setting, &Perf::load())
}

pub fn advise_with(hw: &HardwareInfo, gpu_wanted: bool, threads_setting: usize, perf: &Perf) -> Advice {
    let threads = if threads_setting > 0 { threads_setting } else { hw.recommended_threads() };
    let gpu_card = usable_gpu(hw);
    let gpu = gpu_wanted && hw.gpu_build && gpu_card.is_some();
    let integrated = gpu_card.is_some_and(|g| g.integrated);
    let power = cpu_power(hw, threads);
    let mut reasons = Vec::new();

    let blocker = (!hw.avx2).then(|| {
        "Процессор без AVX2: эта сборка whisper.cpp на нём не запустится. Нужен процессор 2013 года или новее.".to_string()
    });

    let guess = |preset: Preset| {
        if gpu { gpu_speed(preset, integrated) } else { reference_speed(preset) * power / REF_POWER }
    };
    // Замер одного режима говорит о машине в целом: во сколько раз она быстрее
    // или медленнее нашей оценки. Тем же множителем правим незамеренные режимы.
    let ratios: Vec<f64> = ORDER
        .iter()
        .filter_map(|&p| perf.speed(p, threads, gpu).map(|m| m / guess(p).max(1e-6)))
        .collect();
    let calibration = if ratios.is_empty() { 1.0 } else { ratios.iter().sum::<f64>() / ratios.len() as f64 };

    let estimates: Vec<Estimate> = ORDER
        .iter()
        .map(|&preset| {
            let (speed, measured) = match perf.speed(preset, threads, gpu) {
                Some(s) => (s, true),
                None => (guess(preset) * calibration, false),
            };
            let info = preset.info();
            Estimate {
                preset,
                speed,
                measured,
                // Пока 0 — не знаем памяти, не мешаем выбору.
                fits_ram: hw.ram_available == 0 || hw.ram_available >= ram_need(preset),
                fits_disk: preset.is_installed() || hw.disk_free.is_none_or(|free| free >= info.size + (200 << 20)),
                installed: preset.is_installed(),
            }
        })
        .collect();

    let mut chosen = Preset::Fast;
    for e in &estimates {
        let title = e.preset.info().title;
        if !e.fits_ram {
            reasons.push(format!("«{title}» не влезет в свободную память ({} свободно)", gb(hw.ram_available)));
            continue;
        }
        if !e.fits_disk {
            reasons.push(format!("для «{title}» мало места на диске"));
            continue;
        }
        if e.speed < min_speed(e.preset) {
            continue;
        }
        chosen = e.preset;
        break;
    }

    let speed = estimates.iter().find(|e| e.preset == chosen).map(|e| e.speed).unwrap_or(0.0);
    let device = if gpu {
        gpu_card.map(|g| g.name.clone()).unwrap_or_else(|| "видеокарта".into())
    } else {
        format!("{threads} {} процессора", plural(threads, "поток", "потока", "потоков"))
    };
    reasons.insert(
        0,
        format!(
            "{device}: час записи ≈ {} ({}×{})",
            human_minutes(3600.0 / speed.max(0.01)),
            if estimates.iter().any(|e| e.preset == chosen && e.measured) { "замер на этом ПК, " } else { "" },
            fmt1(speed)
        ),
    );
    if hw.efficiency_cores > 0 && threads_setting == 0 {
        reasons.push(format!("E-ядра ({}) не задействованы: они тормозят общий счёт", hw.efficiency_cores));
    }

    if hw.power.on_battery && hw.power.battery_percent.is_some_and(|p| p < 50) {
        let lighter = match chosen {
            Preset::Accurate => Some(Preset::Balanced),
            Preset::Balanced => Some(Preset::Fast),
            Preset::Fast => None,
        };
        if let Some(l) = lighter {
            reasons.push(format!("ноутбук на батарее ({}%) — режим полегче", hw.power.battery_percent.unwrap_or(0)));
            chosen = l;
        }
    }

    Advice { preset: chosen, threads, gpu, estimates, reasons, blocker }
}

/// Дискретная карта от 3 ГБ, а если её нет — встроенная при 16 ГБ общей памяти:
/// Radeon 780M/880M и Intel Arc берут память у системы.
fn usable_gpu(hw: &HardwareInfo) -> Option<&crate::hardware::Gpu> {
    hw.best_gpu()
        .filter(|g| g.vram >= 3 << 30)
        .or_else(|| (hw.ram_total >= 15 << 30).then(|| hw.gpus.iter().find(|g| g.integrated)).flatten())
}

// ───────────────────────── Замеры ─────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Sample {
    /// × к реальному времени, сглаженное.
    speed: f64,
    threads: usize,
    count: u32,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Perf {
    #[serde(default)]
    samples: HashMap<String, Sample>,
}

impl Perf {
    fn key(preset: Preset, gpu: bool) -> String {
        format!("{preset:?}{}", if gpu { "-gpu" } else { "" }).to_lowercase()
    }

    pub fn load() -> Self {
        std::fs::read_to_string(paths::data_dir().join("perf.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    fn speed(&self, preset: Preset, threads: usize, gpu: bool) -> Option<f64> {
        let s = self.samples.get(&Self::key(preset, gpu))?;
        if gpu || s.threads == threads {
            return Some(s.speed);
        }
        // Замер при другом числе потоков — пересчитываем с той же нелинейностью.
        Some(s.speed * (threads as f64 / s.threads.max(1) as f64).powf(0.85))
    }

    /// Записывает реальную скорость. Записи короче окна whisper (30 с) не учитываем:
    /// окно всегда считается целиком, и клип на 9 с выглядел бы втрое медленнее.
    pub fn record(preset: Preset, threads: usize, gpu: bool, audio: f64, compute: f64) {
        if audio < 30.0 || compute <= 0.0 {
            return;
        }
        let speed = audio / compute;
        let mut perf = Self::load();
        let key = Self::key(preset, gpu);
        let entry = perf.samples.entry(key).or_insert(Sample { speed, threads, count: 0 });
        if entry.threads != threads {
            *entry = Sample { speed, threads, count: 0 };
        }
        // Скользящее среднее: одна запись с длинной тишиной не перекосит оценку.
        entry.speed = if entry.count == 0 { speed } else { entry.speed * 0.7 + speed * 0.3 };
        entry.count += 1;
        if let Ok(json) = serde_json::to_vec_pretty(&perf) {
            let _ = std::fs::write(paths::data_dir().join("perf.json"), json);
        }
    }
}

fn gb(bytes: u64) -> String {
    format!("{} ГБ", fmt1(bytes as f64 / (1u64 << 30) as f64))
}

fn fmt1(v: f64) -> String {
    format!("{v:.1}").replace('.', ",")
}

fn human_minutes(sec: f64) -> String {
    let m = (sec / 60.0).round() as u64;
    if m >= 60 {
        format!("{} ч {:02} мин", m / 60, m % 60)
    } else {
        format!("{} мин", m.max(1))
    }
}

fn plural(n: usize, one: &'static str, few: &'static str, many: &'static str) -> &'static str {
    let (a, b) = (n % 10, n % 100);
    if a == 1 && b != 11 {
        one
    } else if (2..=4).contains(&a) && !(12..=14).contains(&b) {
        few
    } else {
        many
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::{Gpu, Power};

    fn advise(hw: &HardwareInfo, gpu: bool, threads: usize) -> Advice {
        advise_with(hw, gpu, threads, &Perf::default())
    }

    fn machine(p: usize, e: usize, ram_gb: u64) -> HardwareInfo {
        HardwareInfo {
            cpu: "test".into(),
            physical_cores: p + e,
            logical_cores: (p + e) * 2,
            performance_cores: p,
            efficiency_cores: e,
            avx2: true,
            avx512: false,
            fma: true,
            ram_total: ram_gb << 30,
            ram_available: (ram_gb << 30) / 2,
            gpus: vec![Gpu { name: "iGPU".into(), vram: 0, integrated: true }],
            npu: None,
            power: Power { on_battery: false, battery_percent: None },
            disk_free: Some(100 << 30),
            gpu_build: false,
            whisper_cpp: "test".into(),
        }
    }

    #[test]
    fn weak_laptop_gets_fast() {
        let a = advise(&machine(2, 0, 8), false, 0);
        assert_eq!(a.preset, Preset::Fast);
    }

    #[test]
    fn six_cores_get_balanced() {
        let a = advise(&machine(6, 0, 16), false, 0);
        assert!(matches!(a.preset, Preset::Balanced | Preset::Accurate));
        assert_eq!(a.threads, 6);
    }

    #[test]
    fn low_memory_steps_down() {
        let mut hw = machine(16, 0, 32);
        hw.ram_available = 1300 << 20;
        let a = advise(&hw, false, 0);
        assert_eq!(a.preset, Preset::Fast);
    }

    #[test]
    fn battery_steps_down() {
        let mut hw = machine(6, 0, 16);
        let plugged = advise(&hw, false, 0).preset;
        hw.power = Power { on_battery: true, battery_percent: Some(30) };
        let on_battery = advise(&hw, false, 0).preset;
        assert_ne!(plugged, on_battery);
    }

    #[test]
    fn hybrid_uses_p_cores_only() {
        let a = advise(&machine(6, 8, 32), false, 0);
        assert_eq!(a.threads, 6);
    }

    #[test]
    fn measurement_calibrates_other_presets() {
        let hw = machine(6, 0, 16);
        let mut perf = Perf::default();
        perf.samples.insert(Perf::key(Preset::Fast, false), Sample { speed: reference_speed(Preset::Fast) / 2.0, threads: 6, count: 1 });
        let a = advise_with(&hw, false, 0, &perf);
        let fast = a.estimate(Preset::Fast).unwrap();
        let balanced = a.estimate(Preset::Balanced).unwrap();
        assert!(fast.measured && !balanced.measured);
        // Машина вдвое медленнее оценки — «Баланс» тоже должен подешеветь вдвое.
        assert!(balanced.speed < fast.speed);
    }

    #[test]
    fn gpu_is_used_but_accurate_stays_manual() {
        let mut hw = machine(6, 0, 16);
        hw.gpu_build = true;
        hw.gpus = vec![Gpu { name: "RTX".into(), vram: 8 << 30, integrated: false }];
        let a = advise(&hw, true, 0);
        assert!(a.gpu);
        assert_eq!(a.preset, Preset::Balanced);
        assert!(a.estimate(Preset::Accurate).unwrap().speed > 2.0);
    }

    #[test]
    fn no_avx2_blocks() {
        let mut hw = machine(4, 0, 8);
        hw.avx2 = false;
        assert!(advise(&hw, false, 0).blocker.is_some());
    }
}

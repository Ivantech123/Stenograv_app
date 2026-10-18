//! Что умеет этот компьютер. Всё через WinAPI и реестр — без PowerShell
//! и WMI: опрос занимает миллисекунды, окно открывается сразу.

use std::path::Path;

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Gpu {
    pub name: String,
    /// Байт видеопамяти; 0 — драйвер не сообщил.
    pub vram: u64,
    /// Встроенная графика делит обычную память и мало что ускоряет.
    pub integrated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Power {
    pub on_battery: bool,
    /// 0–100, None — батареи нет или заряд неизвестен.
    pub battery_percent: Option<u8>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HardwareInfo {
    pub cpu: String,
    pub physical_cores: usize,
    pub logical_cores: usize,
    /// Производительные ядра. На гибридных Intel (12-е поколение и новее) это
    /// меньше physical_cores: E-ядра тормозят общий счёт whisper.cpp.
    pub performance_cores: usize,
    pub efficiency_cores: usize,
    pub avx2: bool,
    pub avx512: bool,
    pub fma: bool,
    pub ram_total: u64,
    pub ram_available: u64,
    pub gpus: Vec<Gpu>,
    pub npu: Option<String>,
    pub power: Power,
    /// Свободно на диске, где лежат модели.
    pub disk_free: Option<u64>,
    pub gpu_build: bool,
    pub whisper_cpp: String,
}

impl HardwareInfo {
    /// Потоки для whisper.cpp: только производительные ядра. SMT-соседи делят
    /// одни AVX-блоки, а E-ядро держит всех в барьере синхронизации.
    pub fn recommended_threads(&self) -> usize {
        let cores = if self.performance_cores > 0 { self.performance_cores } else { self.physical_cores };
        // Выше 12 потоков whisper почти не ускоряется, а система начинает тормозить.
        cores.clamp(1, 12)
    }

    /// Дискретная видеокарта с памятью, которой хватит на модель.
    pub fn best_gpu(&self) -> Option<&Gpu> {
        self.gpus.iter().filter(|g| !g.integrated).max_by_key(|g| g.vram)
    }
}

pub fn inspect() -> HardwareInfo {
    let (performance_cores, efficiency_cores) = core_classes();
    let (ram_total, ram_available) = memory();
    HardwareInfo {
        cpu: cpu_brand().unwrap_or_else(|| "Неизвестный процессор".into()),
        physical_cores: num_cpus::get_physical(),
        logical_cores: num_cpus::get(),
        performance_cores,
        efficiency_cores,
        avx2: feature("avx2"),
        avx512: feature("avx512f"),
        fma: feature("fma"),
        ram_total,
        ram_available,
        gpus: gpus(),
        npu: npu(),
        power: power(),
        disk_free: disk_free(&crate::paths::models_dir()),
        gpu_build: cfg!(feature = "gpu"),
        whisper_cpp: whisper_rs::WHISPER_CPP_VERSION.to_string(),
    }
}

fn feature(name: &str) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        match name {
            "avx2" => std::is_x86_feature_detected!("avx2"),
            "avx512f" => std::is_x86_feature_detected!("avx512f"),
            "fma" => std::is_x86_feature_detected!("fma"),
            _ => false,
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = name;
        false
    }
}

#[cfg(target_arch = "x86_64")]
fn cpu_brand() -> Option<String> {
    use std::arch::x86_64::__cpuid;
    // Листы 0x8000_0002..4 — строка бренда процессора.
    if __cpuid(0x8000_0000).eax < 0x8000_0004 {
        return None;
    }
    let mut bytes = Vec::with_capacity(48);
    for leaf in 0x8000_0002u32..=0x8000_0004 {
        let r = __cpuid(leaf);
        for reg in [r.eax, r.ebx, r.ecx, r.edx] {
            bytes.extend_from_slice(&reg.to_le_bytes());
        }
    }
    let s = String::from_utf8_lossy(&bytes).trim_matches(char::from(0)).trim().to_string();
    (!s.is_empty()).then_some(s)
}

#[cfg(not(target_arch = "x86_64"))]
fn cpu_brand() -> Option<String> {
    None
}

// ───────────────────────── Windows ─────────────────────────

/// (производительные, энергоэффективные). Windows помечает ядра EfficiencyClass:
/// у P-ядер класс выше. На негибридном процессоре все ядра в одном классе.
#[cfg(windows)]
fn core_classes() -> (usize, usize) {
    use windows_sys::Win32::System::SystemInformation::{
        GetLogicalProcessorInformationEx, RelationProcessorCore, SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
    };
    let mut len = 0u32;
    unsafe { GetLogicalProcessorInformationEx(RelationProcessorCore, std::ptr::null_mut(), &mut len) };
    if len == 0 {
        return (num_cpus::get_physical(), 0);
    }
    let mut buf = vec![0u8; len as usize];
    let ok = unsafe { GetLogicalProcessorInformationEx(RelationProcessorCore, buf.as_mut_ptr().cast(), &mut len) };
    if ok == 0 {
        return (num_cpus::get_physical(), 0);
    }
    let mut classes = Vec::new();
    let mut offset = 0usize;
    while offset + std::mem::size_of::<u32>() * 2 <= len as usize {
        // SAFETY: Windows кладёт записи переменной длины подряд; Size — длина текущей.
        let item = unsafe { &*(buf.as_ptr().add(offset) as *const SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX) };
        if item.Size == 0 {
            break;
        }
        if item.Relationship == RelationProcessorCore {
            classes.push(unsafe { item.Anonymous.Processor.EfficiencyClass });
        }
        offset += item.Size as usize;
    }
    let Some(&top) = classes.iter().max() else { return (num_cpus::get_physical(), 0) };
    let p = classes.iter().filter(|&&c| c == top).count();
    (p, classes.len() - p)
}

#[cfg(windows)]
fn memory() -> (u64, u64) {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut m: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    m.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    if unsafe { GlobalMemoryStatusEx(&mut m) } == 0 {
        return (0, 0);
    }
    (m.ullTotalPhys, m.ullAvailPhys)
}

#[cfg(windows)]
fn power() -> Power {
    use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    let mut s: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
    if unsafe { GetSystemPowerStatus(&mut s) } == 0 {
        return Power { on_battery: false, battery_percent: None };
    }
    // ACLineStatus: 0 — от батареи, 1 — от сети, 255 — неизвестно. 128 в флагах — батареи нет.
    let has_battery = s.BatteryFlag != 128 && s.BatteryFlag != 255;
    Power {
        on_battery: s.ACLineStatus == 0,
        battery_percent: (has_battery && s.BatteryLifePercent <= 100).then_some(s.BatteryLifePercent),
    }
}

#[cfg(windows)]
fn disk_free(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut free = 0u64;
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) };
    (ok != 0).then_some(free)
}

/// Видеоадаптеры из класса Display. Виртуальные (Remote Display, Basic Render) отбрасываем.
#[cfg(windows)]
fn gpus() -> Vec<Gpu> {
    const DISPLAY: &str = r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
    let mut out: Vec<Gpu> = Vec::new();
    for key in class_devices(DISPLAY) {
        let Ok(name) = key.get_value::<String, _>("DriverDesc") else { continue };
        let lower = name.to_lowercase();
        if ["basic render", "basic display", "remote display", "virtual", "parsec", "meta"].iter().any(|s| lower.contains(s)) {
            continue;
        }
        let vram = key
            .get_raw_value("HardwareInformation.qwMemorySize")
            .ok()
            .and_then(|v| v.bytes.get(..8).map(|b| u64::from_le_bytes(b.try_into().unwrap())))
            .or_else(|| {
                key.get_raw_value("HardwareInformation.MemorySize")
                    .ok()
                    .and_then(|v| v.bytes.get(..4).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as u64))
            })
            .unwrap_or(0);
        let integrated = is_integrated(&lower, vram);
        if !out.iter().any(|g| g.name == name) {
            out.push(Gpu { name, vram, integrated });
        }
    }
    out
}

fn is_integrated(lower: &str, vram: u64) -> bool {
    const IGPU: &[&str] = &["intel(r) uhd", "intel(r) hd", "intel(r) iris", "intel(r) arc(tm) graphics", "radeon(tm) graphics", "radeon 6", "radeon 7", "radeon 8", "vega"];
    let named_discrete = lower.contains("geforce") || lower.contains("rtx") || lower.contains(" rx ") || lower.contains("arc(tm) a");
    !named_discrete && (IGPU.iter().any(|s| lower.contains(s)) || (vram > 0 && vram < 1 << 30))
}

/// NPU (Ryzen AI, Intel AI Boost, Snapdragon) — класс ComputeAccelerator.
#[cfg(windows)]
fn npu() -> Option<String> {
    const ACCEL: &str = r"SYSTEM\CurrentControlSet\Control\Class\{f01a9d53-3ff6-48d2-9f97-c8a7004be10c}";
    class_devices(ACCEL).into_iter().find_map(|k| k.get_value::<String, _>("DriverDesc").ok())
}

#[cfg(windows)]
fn class_devices(class: &str) -> Vec<winreg::RegKey> {
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ};
    let Ok(root) = winreg::RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey_with_flags(class, KEY_READ) else {
        return Vec::new();
    };
    root.enum_keys()
        .flatten()
        .filter(|name| name.chars().all(|c| c.is_ascii_digit()))
        .filter_map(|name| root.open_subkey_with_flags(name, KEY_READ).ok())
        .collect()
}

// ───────────────────────── Не Windows ─────────────────────────

#[cfg(not(windows))]
fn core_classes() -> (usize, usize) {
    (num_cpus::get_physical(), 0)
}
#[cfg(not(windows))]
fn memory() -> (u64, u64) {
    (0, 0)
}
#[cfg(not(windows))]
fn power() -> Power {
    Power { on_battery: false, battery_percent: None }
}
#[cfg(not(windows))]
fn disk_free(_path: &Path) -> Option<u64> {
    None
}
#[cfg(not(windows))]
fn gpus() -> Vec<Gpu> {
    Vec::new()
}
#[cfg(not(windows))]
fn npu() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integrated_detection() {
        assert!(is_integrated("amd radeon(tm) graphics", 512 << 20));
        assert!(is_integrated("amd radeon 880m graphics", 0));
        assert!(!is_integrated("nvidia geforce rtx 3060", 12 << 30));
        assert!(is_integrated("intel(r) uhd graphics 770", 128 << 20));
    }

    #[test]
    fn inspect_runs() {
        let hw = inspect();
        assert!(hw.physical_cores >= 1);
        assert!(hw.performance_cores >= 1);
        assert!(hw.recommended_threads() >= 1);
    }
}

//! Пока идёт расшифровка: ноутбук не засыпает, а окно и браузер не тормозят.

#[cfg(windows)]
pub fn busy(on: bool) {
    use windows_sys::Win32::System::Power::{SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED};
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, SetPriorityClass, BELOW_NORMAL_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS,
    };
    unsafe {
        // Двухчасовое заседание не должно оборваться из-за спящего режима.
        SetThreadExecutionState(if on { ES_CONTINUOUS | ES_SYSTEM_REQUIRED } else { ES_CONTINUOUS });
        // Все ядра заняты whisper — уступаем их тому, с чем человек работает сейчас.
        SetPriorityClass(GetCurrentProcess(), if on { BELOW_NORMAL_PRIORITY_CLASS } else { NORMAL_PRIORITY_CLASS });
    }
}

#[cfg(not(windows))]
pub fn busy(_on: bool) {}

//! Сборщики данных. Окна реализуют `Collector`, состояние ПК собирает `SystemCollector`.
//! Новые источники (буфер обмена, микрофон) добавляются отдельными модулями.

use ml_core::Sample;

mod system;
#[cfg(windows)]
mod windows_active;
#[cfg(windows)]
mod windows_clipboard;
#[cfg(windows)]
mod windows_power;

pub use system::SystemCollector;
#[cfg(windows)]
pub use windows_clipboard::ClipboardWatcher;

/// На других системах буфер обмена не отслеживается.
#[cfg(not(windows))]
pub struct ClipboardWatcher;

#[cfg(not(windows))]
impl ClipboardWatcher {
    pub fn new() -> Self {
        Self
    }

    pub fn poll(&mut self) -> Option<String> {
        None
    }
}

pub trait Collector: Send {
    /// Один опрос источника. `None`, если сейчас снять нечего.
    fn sample(&mut self) -> Option<Sample>;
}

#[cfg(windows)]
pub fn default_collector() -> Box<dyn Collector> {
    Box::new(windows_active::ActiveWindowCollector::new())
}

#[cfg(not(windows))]
pub fn default_collector() -> Box<dyn Collector> {
    struct Noop;
    impl Collector for Noop {
        fn sample(&mut self) -> Option<Sample> {
            None
        }
    }
    Box::new(Noop)
}

/// (заряд в процентах, работает ли от батареи). `None`, если батареи нет или данных нет.
#[cfg(windows)]
pub fn power_status() -> (Option<u8>, Option<bool>) {
    windows_power::power_status()
}

#[cfg(not(windows))]
pub fn power_status() -> (Option<u8>, Option<bool>) {
    (None, None)
}

/// Просит Windows не замедлять фоновую программу, иначе опрос может «засыпать»,
/// когда окно скрыто в трее.
#[cfg(windows)]
pub fn disable_power_throttling() {
    windows_power::disable_power_throttling();
}

#[cfg(not(windows))]
pub fn disable_power_throttling() {}

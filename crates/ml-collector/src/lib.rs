//! Сборщики данных. Каждый источник (окна, позже микрофон, буфер обмена)
//! реализует `Collector`, поэтому новые источники добавляются без правки остального кода.

use ml_core::Sample;

#[cfg(windows)]
mod windows_active;

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

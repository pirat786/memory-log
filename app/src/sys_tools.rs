pub mod junk;
pub mod lock_watch;
pub mod power;
pub mod win;

pub use junk::{clean_junk, scan_junk, CleanResult, JunkCategory};
pub use lock_watch::spawn_lock_watch;
pub use power::{PowerGuard, PowerStatus};
#[cfg(test)]
pub use win::{fix_keyboard_layout, fix_keyboard_layout_with_target};
pub use win::{
    execute_power_action, fix_selection_or_word_layout, listening_ports,
    reveal_in_explorer, set_startup_item, startup_items, ListeningPort, StartupItem,
};

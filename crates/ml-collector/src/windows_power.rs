use std::ffi::c_void;
use std::mem::size_of;

use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::Threading::{
    GetCurrentProcess, ProcessPowerThrottling, SetProcessInformation, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
    PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE,
};

pub fn power_status() -> (Option<u8>, Option<bool>) {
    let mut s = SYSTEM_POWER_STATUS::default();
    if unsafe { GetSystemPowerStatus(&mut s) }.is_err() {
        return (None, None);
    }
    // Бит 128: батареи нет (настольный ПК).
    if s.BatteryFlag & 128 != 0 {
        return (None, None);
    }
    let pct = (s.BatteryLifePercent <= 100).then_some(s.BatteryLifePercent);
    let on_battery = match s.ACLineStatus {
        0 => Some(true),
        1 => Some(false),
        _ => None,
    };
    (pct, on_battery)
}

pub fn disable_power_throttling() {
    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: 0,
    };
    unsafe {
        let _ = SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            &state as *const _ as *const c_void,
            size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        );
    }
}

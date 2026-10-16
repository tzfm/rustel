//! Whether the operating system asks the CPU to save power.
//!
//! A power-saving mode keeps the clock low for short, regular work such as
//! an audio callback, so the same score can need two to four times the DSP
//! time. Linux reports the mode in `/sys`, and Windows reports it as its
//! effective power mode. Other platforms report nothing.

/// `Some(true)` when the CPU runs in a power-saving mode, `Some(false)` when
/// it does not, and `None` when the platform does not say.
///
/// Cheap enough to call from a UI or engine thread. Never call it from the
/// audio callback: on Linux it reads files.
pub fn power_saving() -> Option<bool> {
    platform::power_saving()
}

/// Reads the laptop's platform profile, the CPU's energy preference and the
/// frequency governor. Any one that asks for power saving decides.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn linux_power_saving(
    profile: Option<&str>,
    preference: Option<&str>,
    governor: Option<&str>,
) -> Option<bool> {
    let profile = profile.map(|p| matches!(p.trim(), "low-power" | "quiet" | "cool"));
    let preference = preference.map(|p| matches!(p.trim(), "power" | "balance_power"));
    // With an energy preference, the CPU picks its own clock and `powersave`
    // is the normal governor name. Without one, `powersave` holds the lowest
    // clock.
    let governor = match preference {
        Some(_) => None,
        None => governor.map(|g| g.trim() == "powersave"),
    };
    [profile, preference, governor]
        .into_iter()
        .flatten()
        .reduce(|a, b| a || b)
}

#[cfg(target_os = "linux")]
mod platform {
    pub(super) fn power_saving() -> Option<bool> {
        let read = |path: &str| std::fs::read_to_string(path).ok();
        super::linux_power_saving(
            read("/sys/firmware/acpi/platform_profile").as_deref(),
            read("/sys/devices/system/cpu/cpu0/cpufreq/energy_performance_preference").as_deref(),
            read("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor").as_deref(),
        )
    }
}

#[cfg(windows)]
mod platform {
    use std::ffi::c_void;
    use std::sync::Once;
    use std::sync::atomic::{AtomicI32, Ordering};

    use windows_sys::Win32::System::Power::{
        EFFECTIVE_POWER_MODE, EFFECTIVE_POWER_MODE_V2, EffectivePowerModeBatterySaver,
        EffectivePowerModeBetterBattery, PowerRegisterForEffectivePowerModeNotifications,
    };

    /// No report has arrived yet.
    const UNKNOWN: i32 = -1;
    static MODE: AtomicI32 = AtomicI32::new(UNKNOWN);

    /// Subscribes on the first call. Windows 10 version 1809 and later then
    /// report the current mode, and report it again on each change. The
    /// subscription lasts as long as the process.
    pub(super) fn power_saving() -> Option<bool> {
        static SUBSCRIBE: Once = Once::new();
        SUBSCRIBE.call_once(|| {
            let mut registration = std::ptr::null_mut();
            // SAFETY: the callback only stores an integer, the context is
            // unused, and `registration` is a valid out-pointer.
            unsafe {
                PowerRegisterForEffectivePowerModeNotifications(
                    EFFECTIVE_POWER_MODE_V2,
                    Some(on_change),
                    std::ptr::null(),
                    &mut registration,
                );
            }
        });
        match MODE.load(Ordering::Relaxed) {
            UNKNOWN => None,
            mode => Some(
                mode == EffectivePowerModeBatterySaver || mode == EffectivePowerModeBetterBattery,
            ),
        }
    }

    unsafe extern "system" fn on_change(mode: EFFECTIVE_POWER_MODE, _context: *const c_void) {
        MODE.store(mode, Ordering::Relaxed);
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod platform {
    pub(super) fn power_saving() -> Option<bool> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saving_profile_or_preference_counts() {
        assert_eq!(
            linux_power_saving(Some("low-power\n"), None, None),
            Some(true)
        );
        assert_eq!(linux_power_saving(Some("quiet\n"), None, None), Some(true));
        assert_eq!(
            linux_power_saving(Some("balanced\n"), Some("power\n"), Some("powersave\n")),
            Some(true)
        );
        assert_eq!(
            linux_power_saving(None, Some("balance_power\n"), None),
            Some(true)
        );
    }

    #[test]
    fn powersave_counts_only_without_an_energy_preference() {
        assert_eq!(
            linux_power_saving(
                Some("performance\n"),
                Some("performance\n"),
                Some("powersave\n")
            ),
            Some(false)
        );
        assert_eq!(
            linux_power_saving(None, Some("balance_performance\n"), Some("powersave\n")),
            Some(false)
        );
        assert_eq!(
            linux_power_saving(None, None, Some("powersave\n")),
            Some(true)
        );
        assert_eq!(
            linux_power_saving(None, None, Some("schedutil\n")),
            Some(false)
        );
    }

    #[test]
    fn no_power_files_is_unknown() {
        assert_eq!(linux_power_saving(None, None, None), None);
    }

    #[test]
    fn the_platform_answer_never_panics() {
        let _ = power_saving();
    }
}

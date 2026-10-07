//! Compact formatting for process and tempo facts in Studio.

/// Render a byte count the way a status bar wants it: three significant
/// figures and a unit, never a twelve-digit number.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [(&str, u64); 4] = [
        ("GB", 1024 * 1024 * 1024),
        ("MB", 1024 * 1024),
        ("kB", 1024),
        ("B", 1),
    ];
    for (unit, scale) in UNITS {
        if bytes >= scale {
            let value = bytes as f64 / scale as f64;
            return if value >= 100.0 || scale == 1 {
                format!("{value:.0}{unit}")
            } else if value >= 10.0 {
                format!("{value:.1}{unit}")
            } else {
                format!("{value:.2}{unit}")
            };
        }
    }
    "0B".to_owned()
}

/// Render a CPU share the way a status bar wants it: whole percents once
/// they are one, a single decimal while the process is under one - a
/// quiescent studio sits near 0.1%, and "0%" for that reads as dead -
/// and plain zero only when there is genuinely nothing to show.
pub fn format_cpu_percent(value: f32) -> String {
    if value < 0.1 {
        "0%".to_owned()
    } else if value < 1.0 {
        format!("{value:.1}%")
    } else {
        format!("{value:.0}%")
    }
}

/// Beats per minute for a cycles-per-second tempo.
///
/// The clock is cycles, not beats. The shared convention is four beats to a
/// cycle, matching `setcpm(bpm/4)`.
pub fn beats_per_minute(cps: f64) -> f64 {
    cps * 60.0 * 4.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_formatting_stays_short_at_every_scale() {
        assert_eq!(format_bytes(0), "0B");
        assert_eq!(format_bytes(512), "512B");
        assert_eq!(format_bytes(1536), "1.50kB");
        assert_eq!(format_bytes(84 * 1024 * 1024), "84.0MB");
        assert_eq!(format_bytes(700 * 1024 * 1024), "700MB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3.00GB");
    }

    #[test]
    fn cpu_percent_shows_one_decimal_only_under_one_percent() {
        // Whole percents stay whole, whatever the decimals.
        assert_eq!(format_cpu_percent(99.0), "99%");
        assert_eq!(format_cpu_percent(12.6), "13%");
        assert_eq!(format_cpu_percent(0.99), "1.0%");
        // Under one percent the first decimal is the story: an idle
        // studio is not dead, it is quiet.
        assert_eq!(format_cpu_percent(0.5), "0.5%");
        assert_eq!(format_cpu_percent(0.1), "0.1%");
        assert_eq!(format_cpu_percent(0.14), "0.1%");
        // Below a tenth of a percent the decimal would only say 0.0.
        assert_eq!(format_cpu_percent(0.09), "0%");
        assert_eq!(format_cpu_percent(0.0), "0%");
    }

    #[test]
    fn tempo_uses_the_four_beats_per_cycle_convention() {
        assert!((beats_per_minute(0.5) - 120.0).abs() < 1e-9);
        assert!((beats_per_minute(1.0) - 240.0).abs() < 1e-9);
    }
}

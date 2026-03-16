/// Format a nanosecond value into a human-readable duration string.
/// Uses the largest unit where value >= 1: s, ms, us, ns.
pub fn format_nanoseconds(ns: i64) -> String {
    let abs = ns.unsigned_abs();
    let sign = if ns < 0 { "-" } else { "" };

    if abs >= 1_000_000_000 {
        format!("{sign}{:.1}s", abs as f64 / 1_000_000_000.0)
    } else if abs >= 1_000_000 {
        format!("{sign}{:.0}ms", abs as f64 / 1_000_000.0)
    } else if abs >= 1_000 {
        format!("{sign}{:.0}us", abs as f64 / 1_000.0)
    } else {
        format!("{sign}{abs}ns")
    }
}

/// Format a byte value into a human-readable size string.
/// Uses the largest unit where value >= 1: GB, MB, KB, B.
pub fn format_bytes(bytes: i64) -> String {
    let abs = bytes.unsigned_abs();
    let sign = if bytes < 0 { "-" } else { "" };

    if abs >= 1_000_000_000 {
        format!("{sign}{:.1}GB", abs as f64 / 1_000_000_000.0)
    } else if abs >= 1_000_000 {
        format!("{sign}{:.0}MB", abs as f64 / 1_000_000.0)
    } else if abs >= 1_000 {
        format!("{sign}{:.0}KB", abs as f64 / 1_000.0)
    } else {
        format!("{sign}{abs}B")
    }
}

/// Format a count value with comma-separated thousands.
pub fn format_count(count: i64) -> String {
    if count == 0 {
        return "0".to_string();
    }

    let sign = if count < 0 { "-" } else { "" };
    let abs = count.unsigned_abs();
    let s = abs.to_string();
    let mut result = String::with_capacity(s.len() + s.len() / 3);

    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            result.push(',');
        }
        result.push(ch);
    }

    format!("{sign}{result}")
}

/// Format a percentage value: "(34.2%)" relative to total.
/// Returns empty string if total is zero.
pub fn format_percent(value: i64, total: i64) -> String {
    if total == 0 {
        return String::new();
    }
    let pct = (value as f64 / total as f64) * 100.0;
    format!("({:.1}%)", pct)
}

/// The unit type for a profile's values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueUnit {
    Nanoseconds,
    Bytes,
    Count,
}

/// Format a value according to its unit.
pub fn format_value(value: i64, unit: ValueUnit) -> String {
    match unit {
        ValueUnit::Nanoseconds => format_nanoseconds(value),
        ValueUnit::Bytes => format_bytes(value),
        ValueUnit::Count => format_count(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Nanoseconds
    #[test]
    fn test_format_ns_seconds() {
        assert_eq!(format_nanoseconds(1_200_000_000), "1.2s");
        assert_eq!(format_nanoseconds(5_500_000_000), "5.5s");
    }

    #[test]
    fn test_format_ns_milliseconds() {
        assert_eq!(format_nanoseconds(340_000_000), "340ms");
        assert_eq!(format_nanoseconds(1_500_000), "2ms");
    }

    #[test]
    fn test_format_ns_microseconds() {
        assert_eq!(format_nanoseconds(15_000), "15us");
        assert_eq!(format_nanoseconds(1_500), "2us");
    }

    #[test]
    fn test_format_ns_nanoseconds() {
        assert_eq!(format_nanoseconds(800), "800ns");
        assert_eq!(format_nanoseconds(0), "0ns");
    }

    // Bytes
    #[test]
    fn test_format_bytes_gb() {
        assert_eq!(format_bytes(1_500_000_000), "1.5GB");
    }

    #[test]
    fn test_format_bytes_mb() {
        assert_eq!(format_bytes(230_000_000), "230MB");
    }

    #[test]
    fn test_format_bytes_kb() {
        assert_eq!(format_bytes(12_000), "12KB");
    }

    #[test]
    fn test_format_bytes_b() {
        assert_eq!(format_bytes(512), "512B");
        assert_eq!(format_bytes(0), "0B");
    }

    // Counts
    #[test]
    fn test_format_count_with_commas() {
        assert_eq!(format_count(1_234_567), "1,234,567");
        assert_eq!(format_count(1_000), "1,000");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(0), "0");
    }

    #[test]
    fn test_format_count_negative() {
        assert_eq!(format_count(-1_234), "-1,234");
    }

    // Percentages
    #[test]
    fn test_format_percent() {
        assert_eq!(format_percent(342, 1000), "(34.2%)");
        assert_eq!(format_percent(1000, 1000), "(100.0%)");
        assert_eq!(format_percent(0, 1000), "(0.0%)");
    }

    #[test]
    fn test_format_percent_zero_total() {
        assert_eq!(format_percent(100, 0), "");
    }

    // format_value dispatch
    #[test]
    fn test_format_value() {
        assert_eq!(format_value(1_200_000_000, ValueUnit::Nanoseconds), "1.2s");
        assert_eq!(format_value(1_500_000_000, ValueUnit::Bytes), "1.5GB");
        assert_eq!(format_value(1_234_567, ValueUnit::Count), "1,234,567");
    }
}

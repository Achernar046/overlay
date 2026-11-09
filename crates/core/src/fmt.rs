/// ฟอร์แมตจำนวน bytes เป็นข้อความอ่านง่าย (binary units เหมือน Task Manager)
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 7] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    let mut v = bytes as f64;
    for unit in UNITS {
        if v < 1024.0 {
            return format!("{v:.1}{unit}");
        }
        v /= 1024.0;
    }
    format!("{v:.1}MiBi")
}

/// ฟอร์แมตความเร็ว (bytes/second)
pub fn format_rate(bps: f64) -> String {
    format!("{}/s", format_bytes(bps as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_bytes() {
        assert_eq!(format_bytes(500), "500.0B");
        assert_eq!(format_bytes(1024 * 1024 + 2048), "1.0MiB");
        assert_eq!(format_bytes(17 * 1024u64.pow(3)), "17.0GiB");
    }

    #[test]
    fn formats_rate() {
        assert_eq!(format_rate(2048.0), "2.0KiB/s");
    }
}

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// ช่วงเวลา sampling (มิลลิวินาที) — plan: 1s (live) / 5s (background)
    pub sample_interval_ms: u64,
    pub collector: CollectorConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CollectorConfig {
    pub monitor_disks: bool,
    pub monitor_networks: bool,
    /// อ่านอุณหภูมิ (บางเครื่องต้องสิทธิ์ admin/root)
    pub monitor_temps: bool,
}

impl Default for CollectorConfig {
    fn default() -> Self {
        Self {
            monitor_disks: true,
            monitor_networks: true,
            monitor_temps: true,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sample_interval_ms: 1000,
            collector: CollectorConfig::default(),
        }
    }
}

impl Config {
    /// อ่านจากไฟล์ TOML ถ้ามี — ไฟล์หาย/แปลงไม่ได้ → ใช้ default แล้วแจ้งเหตุผลกลับ
    pub fn load_or_default(path: impl AsRef<Path>, warn: &mut dyn FnMut(&str)) -> Self {
        let path = path.as_ref();
        if !path.exists() {
            return Self::default();
        }
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                warn(&format!("อ่าน {} ไม่สำเร็จ ({e}) → ใช้ค่า default", path.display()));
                return Self::default();
            }
        };
        match toml::from_str(&text) {
            Ok(c) => c,
            Err(e) => {
                warn(&format!("parse {} ไม่สำเร็จ ({e}) → ใช้ค่า default", path.display()));
                Self::default()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_config() {
        let c: Config = toml::from_str("sample_interval_ms = 500").unwrap();
        assert_eq!(c.sample_interval_ms, 500);
        // ส่วนที่ไม่ได้ระบุได้ค่า default
        assert_eq!(c.collector, CollectorConfig::default());
    }

    #[test]
    fn default_config() {
        let c = Config::default();
        assert_eq!(c.sample_interval_ms, 1000);
        assert!(c.collector.monitor_temps);
    }
}

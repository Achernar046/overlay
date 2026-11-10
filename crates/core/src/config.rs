use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// ช่วงเวลา sampling (มิลลิวินาที) — plan: 1s (live) / 5s (background)
    pub sample_interval_ms: u64,
    pub collector: CollectorConfig,
    pub storage: StorageConfig,
    pub overlay: OverlayConfig, // Phase 5
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CollectorConfig {
    pub monitor_disks: bool,
    pub monitor_networks: bool,
    /// อ่านอุณหภูมิ (บางเครื่องต้องสิทธิ์ admin/root)
    pub monitor_temps: bool,
    /// โหลด NVIDIA NVML ถ้ามี (เติม VRAM/temp ที่ D3DKMT ไม่ให้)
    pub monitor_nvidia: bool,
}

impl Default for CollectorConfig {
    fn default() -> Self {
        Self {
            monitor_disks: true,
            monitor_networks: true,
            monitor_temps: true,
            monitor_nvidia: true,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sample_interval_ms: 1000,
            collector: CollectorConfig::default(),
            storage: StorageConfig::default(),
            overlay: OverlayConfig::default(),
        }
    }
}

/// หน้าต่าง overlay (Phase 5)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct OverlayConfig {
    /// ระยะขอบหน้าต่างจากมุมจอ (point)
    pub margin: f32,
    /// ขนาดเนื้อหา (1.0 = ปกติ) — 0.5–2.0
    pub scale: f32,
    /// "dark" หรือ "light"
    pub theme: String,
    /// ปุ่มลัดกลางระบบ สลับแสดง/ซ่อน overlay (รูปแบบ "ctrl+alt+o")
    pub hotkey: String,
    /// ปุ่มลัดสลับโหมด interactive (หยุด click-through เพื่อลาก/ปิดหน้าต่าง)
    pub interact_hotkey: String,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            margin: 24.0,
            scale: 1.0,
            theme: "dark".into(),
            hotkey: "ctrl+alt+o".into(),
            interact_hotkey: "ctrl+alt+p".into(),
        }
    }
}

/// การเก็บประวัติลง SQLite (Phase 3)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct StorageConfig {
    pub enabled: bool,
    /// พาธของไฟล์ database เทียบกับ working directory
    pub db_path: String,
    /// เขียนลง DB กี่วินาทีครั้ง (plan: 10-30 วิ เป็น transaction เดียว)
    pub flush_secs: u64,
    /// เก็บ raw กี่วันก่อน rollup แล้วลบ (plan: 24 ชม.)
    pub retention_raw_hours: u64,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            db_path: "sysmon.db".into(),
            flush_secs: 15,
            retention_raw_hours: 24,
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

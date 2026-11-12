use smallvec::SmallVec;
use serde::{Deserialize, Serialize};

/// ภาพรวมสถานะเครื่อง ณ ช่วงเวลาหนึ่ง — สิ่งเดียวที่ไหลผ่าน IPC
///
/// หมายเหตุ: `gpus` เป็น list ตั้งแต่ต้นเพื่อรองรับเครื่องที่ใส่ GPU หลายตัว
/// (ตาม plan ส่วน "Multi-GPU")
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Snapshot {
    /// Unix timestamp (milliseconds)
    pub ts: i64,
    pub cpu_total: f32,
    pub cpu_per_core: SmallVec<[f32; 32]>,
    pub cpu_temp: Option<f32>,
    pub mem_used: u64,
    pub mem_total: u64,
    pub swap_used: u64,
    pub disks: SmallVec<[DiskStat; 4]>,
    pub gpus: SmallVec<[GpuStat; 2]>,
    pub nets: SmallVec<[NetStat; 4]>,
}

impl Snapshot {
    pub fn mem_usage_ratio(&self) -> f32 {
        if self.mem_total == 0 {
            0.0
        } else {
            self.mem_used as f32 / self.mem_total as f32
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiskStat {
    /// ป้ายชื่อโวลุ่ม (Windows) / เลข device (Linux)
    pub name: String,
    pub mount: String,
    pub total_bytes: u64,
    pub used_bytes: u64,
}

impl DiskStat {
    pub fn usage_ratio(&self) -> f32 {
        if self.total_bytes == 0 {
            0.0
        } else {
            self.used_bytes as f32 / self.total_bytes as f32
        }
    }
}

/// GPU หนึ่งตัว (เติมค่าจริงตอน Phase 2 ด้วย NVML / PDH / sysfs)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GpuStat {
    pub name: String,
    pub usage_percent: Option<f32>,
    pub mem_used_mb: Option<u64>,
    pub mem_total_mb: Option<u64>,
    pub temp_c: Option<f32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetStat {
    pub name: String,
    /// bytes/second ของช่วงเวลาหลังการ sample ก่อนหน้า
    pub rx_bps: f64,
    pub tx_bps: f64,
    pub rx_total: u64,
    pub tx_total: u64,
}

/// หนึ่งแถวใน Process Monitor — ดึงแยกจาก `Snapshot`
/// (ตาม plan: refresh process list ทุก 2-3 วิ เฉพาะตอนเปิดแท็บ Process)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProcessStat {
    pub pid: u32,
    pub name: String,
    pub cpu_percent: f32,
    pub mem_bytes: u64,
    /// รวมทั้งอ่าน+เขียน ต่อวินาที (bytes) — เติมภายหลังตอน refresh ด้วย disk_usage
    pub io_bps: Option<u64>,
}

/// ตัวเลือกการเรียง Top-N ใน Process Monitor
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ProcessSort {
    #[default]
    Cpu,
    Memory,
    DiskIo,
}

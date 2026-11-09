//! Collector — ดึงค่า CPU/RAM/Disk/GPU/Network เป็น `Snapshot`
//!
//! ตาม plan:
//! - ใช้ `refresh_specifics()` เลือกเฉพาะสิ่งที่ต้องใช้ ไม่ refresh_all()
//! - reuse โครงสร้าง sysinfo ตลอดอายุ Collector ไม่สร้างใหม่ทุก tick
//! - GPU ใช้ sysinfo `gpu` feature (D3DKMT บน Windows) — ง่ายกว่า NVML
//!   และ cross-platform, NVML จะเติมเป็น data พิเศษภายหลังถ้าจำเป็น

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use smallvec::SmallVec;
use sysinfo::{
    Components, CpuRefreshKind, Disks, Gpus, MemoryRefreshKind, Networks, RefreshKind, System,
};

use sysmon_core::{CollectorConfig, DiskStat, GpuStat, NetStat, Snapshot};

pub struct Collector {
    sys: System,
    disks: Option<Disks>,
    nets: Option<Networks>,
    temps: Option<Components>,
    gpus: Option<Gpus>,
    last_tick: Instant,
}

impl Collector {
    /// สร้าง collector ใหม่ พร้อม warm-up เพื่อให้ cpu_usage วัดได้จริงตั้งแต่ snapshot แรก
    pub fn new(config: &CollectorConfig) -> Result<Self, sysinfo::Error> {
        let mut sys = System::new()?;
        sys.refresh_specifics(Self::refresh_kind());

        let disks = config
            .monitor_disks
            .then(|| Disks::new_with_refreshed_list())
            .transpose()?;
        let nets = config
            .monitor_networks
            .then(|| Networks::new_with_refreshed_list())
            .transpose()?;
        let temps = config
            .monitor_temps
            .then(|| Components::new_with_refreshed_list())
            .transpose()?;
        // GPU ไม่มี/เรียกไม่ได้ = ไม่มีข้อมูล ไม่ใช่ fatal
        let gpus = Gpus::new_with_refreshed_list().ok();

        Ok(Self {
            sys,
            disks,
            nets,
            temps,
            gpus,
            last_tick: Instant::now(),
        })
    }

    fn refresh_kind() -> RefreshKind {
        RefreshKind::nothing()
            .with_cpu(CpuRefreshKind::everything())
            .with_memory(MemoryRefreshKind::everything())
    }

    pub fn snapshot(&mut self) -> Snapshot {
        // เวลาต่อ tick ใช้คำนวณ rx/tx เป็น bytes/second
        let dt = self.last_tick.elapsed().as_secs_f64().max(1e-6);
        self.last_tick = Instant::now();

        self.sys.refresh_specifics(Self::refresh_kind());

        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or_default();

        let cpu_total = self.sys.global_cpu_usage();
        let cpu_per_core = self
            .sys
            .cpus()
            .iter()
            .map(|c| c.usage())
            .collect::<SmallVec<[f32; 32]>>();

        Snapshot {
            ts,
            cpu_total,
            cpu_per_core,
            cpu_temp: self.cpu_temp(),
            mem_used: self.sys.used_memory(),
            mem_total: self.sys.total_memory(),
            swap_used: self.sys.used_swap(),
            disks: self.disk_stats(),
            gpus: self.gpu_stats(),
            nets: self.net_stats(dt),
        }
    }

    /// เลือกเซนเซอร์อุณหภูมิ CPU แบบ heuristic (ชื่อขึ้น CPU / Package / Core) — ไม่เจอ = None
    fn cpu_temp(&mut self) -> Option<f32> {
        let temps = self.temps.as_mut()?;
        temps.refresh(false);
        for comp in temps.iter() {
            let name = comp.label().to_ascii_lowercase();
            if (name.contains("cpu") || name.contains("package") || name.contains("core"))
                && let Some(t) = comp.temperature()
            {
                return Some(t);
            }
        }
        None
    }

    fn disk_stats(&mut self) -> SmallVec<[DiskStat; 4]> {
        let mut out = SmallVec::new();
        if let Some(disks) = self.disks.as_mut() {
            disks.refresh(false);
            for d in disks.iter() {
                out.push(DiskStat {
                    name: d.name().to_string_lossy().into_owned(),
                    mount: d.mount_point().to_string_lossy().into_owned(),
                    total_bytes: d.total_space(),
                    used_bytes: d.total_space().saturating_sub(d.available_space()),
                });
            }
        }
        out
    }

    fn gpu_stats(&mut self) -> SmallVec<[GpuStat; 2]> {
        let mut out = SmallVec::new();
        if let Some(gpus) = self.gpus.as_mut() {
            gpus.refresh(false);
            for g in gpus.iter() {
                out.push(GpuStat {
                    name: match (g.vendor(), g.model()) {
                        (Some(v), Some(m)) => format!("{v} {m}"),
                        (Some(v), None) => format!("{v} GPU"),
                        (None, Some(m)) => m.to_string(),
                        (None, None) => "GPU".to_string(),
                    },
                    usage_percent: g.usage(),
                    mem_used_mb: g.used_memory().map(|b| b / (1024 * 1024)),
                    mem_total_mb: g.total_memory().map(|b| b / (1024 * 1024)),
                    temp_c: None, // มาจาก NVML/เซนเซอร์ใน Phase 2
                });
            }
        }
        out
    }

    fn net_stats(&mut self, dt: f64) -> SmallVec<[NetStat; 4]> {
        let mut out = SmallVec::new();
        if let Some(nets) = self.nets.as_mut() {
            nets.refresh(false);
            for (name, data) in nets.iter() {
                out.push(NetStat {
                    name: name.to_string_lossy().into_owned(),
                    rx_bps: data.received() as f64 / dt,
                    tx_bps: data.transmitted() as f64 / dt,
                    rx_total: data.total_received(),
                    tx_total: data.total_transmitted(),
                });
            }
        }
        out
    }
}

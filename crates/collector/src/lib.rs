//! Collector — ดึงค่า CPU/RAM/Disk/GPU/Network เป็น `Snapshot` และ Top-N process
//!
//! ตาม plan:
//! - ใช้ `refresh_specifics()` (และ `refresh_processes`) เลือกเฉพาะที่ใช้ ไม่ refresh_all()
//! - reuse โครงสร้าง sysinfo ตลอดอายุ Collector ไม่สร้างใหม่ทุก tick
//! - GPU baseline ใช้ sysinfo `gpu` feature (D3DKMT บน Windows),
//!   NVIDIA เติมเพิ่มด้วย NVML (temp/VRAM/usage)
//! - process list refresh แยกจาก snapshot เรียกเมื่อจอที่แสดง process เปิดอยู่เท่านั้น

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use nvml_wrapper::enum_wrappers::device::TemperatureSensor;
use smallvec::SmallVec;
use sysinfo::{
    Components, CpuRefreshKind, Disks, Gpus, MemoryRefreshKind, Networks, ProcessesToUpdate,
    RefreshKind, System,
};

use sysmon_core::{
    CollectorConfig, DiskStat, GpuStat, NetStat, ProcessSort, ProcessStat, Snapshot,
};

/// ผูกตัวตน NVIDIA จากชื่อ vendor ที่ sysinfo รายงาน
const NVIDIA_MARKER: &str = "nvidia";

/// ค่า NVML device หนึ่งตัว: (temp °C, mem_used MiB, mem_total MiB, usage %)
type NvmlSample = (Option<u32>, Option<u64>, Option<u64>, Option<f32>);

pub struct Collector {
    sys: System,
    disks: Option<Disks>,
    nets: Option<Networks>,
    temps: Option<Components>,
    gpus: Option<Gpus>,
    nvml: Option<nvml_wrapper::Nvml>,
    last_tick: Instant,
    /// true ตอนยังไม่ผ่านการ sample จริงครั้งแรก — rate อย่าง rx/tx ตอนนี้ไม่น่าเชื่อ
    first_tick: bool,
}

impl Collector {
    /// สร้าง collector ใหม่ พร้อม warm-up เพื่อให้ cpu_usage วัดได้จริงตั้งแต่ snapshot แรก
    pub fn new(config: &CollectorConfig) -> Result<Self, sysinfo::Error> {
        let mut sys = System::new()?;
        sys.refresh_specifics(Self::refresh_kind());

        let disks = config
            .monitor_disks
            .then(Disks::new_with_refreshed_list)
            .transpose()?;
        let nets = config
            .monitor_networks
            .then(Networks::new_with_refreshed_list)
            .transpose()?;
        let temps = config
            .monitor_temps
            .then(Components::new_with_refreshed_list)
            .transpose()?;
        // GPU ไม่มี/เรียกไม่ได้ = ไม่มีข้อมูล ไม่ใช่ fatal
        let gpus = Gpus::new_with_refreshed_list().ok();
        // NVML ใช้ได้เมื่อมี driver NVIDIA — ไม่มี = ข้ามเงียบ ๆ
        let nvml = if config.monitor_nvidia { nvml_wrapper::Nvml::init().ok() } else { None };

        Ok(Self {
            sys,
            disks,
            nets,
            temps,
            gpus,
            nvml,
            last_tick: Instant::now(),
            first_tick: true,
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

        // ตั้งค่า rate แบบ cannot-trust ให้แล้ว และกวาด first_tick ไปเมื่อออกจากฟังก์ชัน
        let rates_trusted = !self.first_tick;
        self.first_tick = false;

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
            nets: self.net_stats(dt, rates_trusted),
        }
    }

    /// Top-N process เรียงตาม `sort` — เรียกแยกจาก `snapshot()` เฉพาะเมื่อจอนั้นเปิดอยู่
    /// (ค่า cpu_usage ต้อง refresh สองรอบขึ้นไปถึงจะมีความหมาย)
    pub fn processes(&mut self, top_n: usize, sort: ProcessSort) -> SmallVec<[ProcessStat; 16]> {
        self.sys.refresh_processes(ProcessesToUpdate::All, true);

        let mut stats: SmallVec<[ProcessStat; 16]> = self
            .sys
            .processes()
            .values()
            // pid 0 = Idle ของ Windows — กรองทิ้ง
            .filter(|p| p.pid().as_u32() != 0)
            .map(|p| ProcessStat {
                pid: p.pid().as_u32(),
                name: p.name().to_string_lossy().into_owned(),
                cpu_percent: p.cpu_usage(),
                mem_bytes: p.memory(),
                io_bps: None, // per-process disk IO เพิ่มภายหลัง
            })
            .collect();

        stats.sort_by(|a: &ProcessStat, b: &ProcessStat| match sort {
            ProcessSort::Cpu => b.cpu_percent.total_cmp(&a.cpu_percent),
            ProcessSort::Memory => b.mem_bytes.cmp(&a.mem_bytes),
        });
        stats.truncate(top_n);
        stats
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
        let mut out: SmallVec<[GpuStat; 2]> = SmallVec::new();
        // ขั้นแรก: ค่าจาก sysinfo (D3DKMT) — จบ borrow ของ self.gpus ก่อนแตะ NVML
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
                    temp_c: None,
                });
            }
        }

        // ขั้นที่สอง: เติมค่า NVIDIA ที่ D3DKMT ไม่ให้ (temp/VRAM/usage)
        // จับคู่เรียงลำดับ: NVML device index i ↔ GPU ลำดับ i ในกลุ่ม NVIDIA
        let mut nvml_index = 0usize;
        for stat in &mut *out {
            if !stat
                .name
                .to_ascii_lowercase()
                .contains(NVIDIA_MARKER)
            {
                continue;
            }
            if let Some((temp, used, total, usage)) = self.nvml_info(nvml_index) {
                stat.temp_c = stat.temp_c.or(temp.map(|t| t as f32));
                if stat.mem_used_mb.is_none_or(|v| v == 0) {
                    stat.mem_used_mb = used;
                }
                if stat.mem_total_mb.is_none_or(|v| v == 0) {
                    stat.mem_total_mb = total;
                }
                if stat.usage_percent.is_none() {
                    stat.usage_percent = usage;
                }
            }
            nvml_index += 1;
        }
        out
    }

    /// ค่า NVML device หนึ่งตัว: temp °C / mem_used / mem_total (MiB) / usage %
    fn nvml_info(&self, index: usize) -> Option<NvmlSample> {
        let nvml = self.nvml.as_ref()?;
        let device = nvml.device_by_index(index as u32).ok()?;
        let temp = device.temperature(TemperatureSensor::Gpu).ok();
        let mem = device
            .memory_info()
            .ok()
            .map(|m| (m.used / (1024 * 1024), m.total / (1024 * 1024)));
        let usage = device.utilization_rates().ok().map(|u| u.gpu as f32);
        Some((temp, mem.map(|m| m.0), mem.map(|m| m.1), usage))
    }

    fn net_stats(&mut self, dt: f64, rates_trusted: bool) -> SmallVec<[NetStat; 4]> {
        let mut out = SmallVec::new();
        if let Some(nets) = self.nets.as_mut() {
            nets.refresh(false);
            for (name, data) in nets.iter() {
                let (rx, tx) = if rates_trusted {
                    (data.received() as f64 / dt, data.transmitted() as f64 / dt)
                } else {
                    (0.0, 0.0) // tick แรก: delta รวมตั้งแต่ init → ค่า rate ปลอม เอา 0 ไปก่อน
                };
                out.push(NetStat {
                    name: name.to_string_lossy().into_owned(),
                    rx_bps: rx,
                    tx_bps: tx,
                    rx_total: data.total_received(),
                    tx_total: data.total_transmitted(),
                });
            }
        }
        out
    }
}

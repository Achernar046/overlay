//! sysmon-ui — egui dashboard (Phase 4)
//!
//! ตามแผน (UI v1):
//! - หน้า **Live**: คุม Collector ในตัวเอง (realtime ไม่ผ่าน IPC ก่อน)
//! - หน้า **ประวัติ**: อ่าน SQLite โดยตรง — 1 ชม. = raw, 24 ชม./30 วัน = rollup 1m, 1 ปี = rollup 1h
//!   (IPC กับ daemon เลื่อนไปทีหลังตาม plan)

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eframe::egui;
use egui_plot::{GridMark, Legend, Line, Plot, PlotPoints, Polygon};

use sysmon_collector::Collector;
use sysmon_core::ipc::SnapshotFeed;
use sysmon_core::{format_bytes, format_rate, Config, Snapshot};
use sysmon_storage::{BucketPoint, RawPoint, Storage};

/// เก็บข้อมูล live ย้อนหลังสูงสุด (จุด) — 600 จุด ≈ 10 นาที ที่ 1 วิ/จุด
const LIVE_POINTS: usize = 600;
/// downsample ประวัติก่อนวาด — กัน 40k+ จุดทำให้ plot ช้า
const HIST_MAX_POINTS: usize = 10_000;

#[derive(Clone, Copy, PartialEq)]
pub enum Tab {
    Live,
    History,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum HistoryRange {
    H1,
    H24,
    D30,
    Y1,
}

impl HistoryRange {
    fn all() -> [HistoryRange; 4] {
        [
            HistoryRange::H1,
            HistoryRange::H24,
            HistoryRange::D30,
            HistoryRange::Y1,
        ]
    }

    fn label(self) -> &'static str {
        match self {
            HistoryRange::H1 => "1 ชั่วโมง",
            HistoryRange::H24 => "24 ชั่วโมง",
            HistoryRange::D30 => "30 วัน",
            HistoryRange::Y1 => "1 ปี",
        }
    }

    /// ช่วงเวลาย้อนหลัง (ms)
    fn span_ms(self) -> i64 {
        match self {
            HistoryRange::H1 => 3_600_000,
            HistoryRange::H24 => 86_400_000,
            HistoryRange::D30 => 86_400_000 * 30,
            HistoryRange::Y1 => 86_400_000 * 365,
        }
    }

    /// คำอธิบายแหล่งข้อมูลตาม plan (raw / rollup 1m / rollup 1h)
    fn source_label(self) -> &'static str {
        match self {
            HistoryRange::H1 => "raw ต่อ sample",
            HistoryRange::H24 | HistoryRange::D30 => "rollup นาที (avg/min/max)",
            HistoryRange::Y1 => "rollup ชั่วโมง (avg/min/max)",
        }
    }

    /// ช่วงยาวพอควร → แกนเวลาแสดงวันที่ด้วย
    fn long_ticks(self) -> bool {
        matches!(self, HistoryRange::D30 | HistoryRange::Y1)
    }
}

/// วิธีอ่านข้อมูลให้ตรงช่วงเวลา (ตาม plan: raw เก็บแค่ 24 ชม. → ช่วงยาวใช้ rollup)
fn hist_query(range: HistoryRange, now_ms: i64) -> HistQuery {
    let since = now_ms - range.span_ms();
    match range {
        HistoryRange::H1 => HistQuery::Raw { since, limit: 5_000 },
        HistoryRange::H24 => HistQuery::Minute { since, limit: 2_000 },
        HistoryRange::D30 => HistQuery::Minute { since, limit: 60_000 },
        HistoryRange::Y1 => HistQuery::Hour { since, limit: 12_000 },
    }
}

enum HistQuery {
    Raw { since: i64, limit: usize },
    Minute { since: i64, limit: usize },
    Hour { since: i64, limit: usize },
}

/// จุดประวัติหนึ่งจุด — normalize จาก raw (avg=min=max=ตัวค่าเดียว) หรือ rollup
#[derive(Clone, Copy, Default)]
struct HistPoint {
    ts: i64,
    cpu_avg: Option<f64>,
    cpu_min: Option<f64>,
    cpu_max: Option<f64>,
    cpu_temp: Option<f64>,
    mem_used_gib: Option<f64>,
    net_rx_bps: Option<f64>,
    net_tx_bps: Option<f64>,
}

impl HistPoint {
    fn from_raw(p: &RawPoint) -> Self {
        Self {
            ts: p.ts,
            cpu_avg: p.cpu_total,
            cpu_min: p.cpu_total,
            cpu_max: p.cpu_total,
            cpu_temp: p.cpu_temp,
            mem_used_gib: p.mem_used.map(|v| v / 1_073_741_824.0),
            net_rx_bps: p.net_rx,
            net_tx_bps: p.net_tx,
        }
    }

    fn from_bucket(p: &BucketPoint) -> Self {
        Self {
            ts: p.ts,
            cpu_avg: p.cpu_avg,
            cpu_min: p.cpu_min,
            cpu_max: p.cpu_max,
            cpu_temp: p.cpu_temp,
            mem_used_gib: p.mem_used.map(|v| v / 1_073_741_824.0),
            net_rx_bps: p.net_rx,
            net_tx_bps: p.net_tx,
        }
    }
}

pub struct SystemMonitorApp {
    tab: Tab,

    // --- live (Collector อยู่ในตัว UI, หรือผ่าน IPC จาก daemon) ---
    feed: Option<SnapshotFeed>,
    feed_retry: Option<Instant>,
    ipc_name: Option<String>,
    collector: Option<Collector>,
    collector_err: Option<String>,
    live: Vec<RawPoint>,
    last_snap: Option<Snapshot>,
    last_sample: Option<Instant>,
    interval: Duration,

    // --- ประวัติ (SQLite) ---
    storage: Option<Storage>,
    storage_err: Option<String>,
    hist_range: HistoryRange,
    hist: Vec<HistPoint>,
    hist_loaded_for: Option<HistoryRange>,
    hist_loaded_at: Option<Instant>,
    hist_err: Option<String>,
}

impl SystemMonitorApp {
    pub fn new(config: Config) -> Self {
        let (collector, collector_err) = match Collector::new(&config.collector) {
            Ok(c) => (Some(c), None),
            Err(e) => (None, Some(e.to_string())),
        };
        let (storage, storage_err) = match Storage::open(&config.storage.db_path) {
            Ok(s) => (Some(s), None),
            Err(e) => (None, Some(format!("{}: {e}", config.storage.db_path))),
        };
        Self {
            interval: Duration::from_millis(config.sample_interval_ms.max(200)),
            tab: Tab::Live,
            feed: None,
            feed_retry: Some(Instant::now()),
            ipc_name: config.ipc.enabled.then_some(config.ipc.name.clone()),
            collector,
            collector_err,
            live: Vec::with_capacity(LIVE_POINTS),
            last_snap: None,
            last_sample: None,
            storage,
            storage_err,
            hist_range: HistoryRange::H1,
            hist: Vec::new(),
            hist_loaded_for: None,
            hist_loaded_at: None,
            hist_err: None,
        }
    }

    // ---------- เก็บข้อมูล live ----------

    /// พยายามต่อ IPC จาก daemon — ได้ข้อมูลสด → ไม่ต้อง sample เอง (ไม่แย่งทรัพยากร)
    fn tick_feed(&mut self) -> bool {
        // ต่อใหม่ทุก 10 วิ ถ้าไม่มี feed (daemon ยังไม่เปิด) หรือ feed ขาดไปแล้ว
        if self.feed.is_none() {
            if let Some(t) = self.feed_retry
                && t.elapsed() < Duration::from_secs(10) {
                    return false;
                }
            if let Some(name) = self.ipc_name.as_deref() {
                self.feed = SnapshotFeed::connect(name).ok();
                self.feed_retry = Some(Instant::now());
            }
        }
        match self.feed.as_ref() {
            Some(f) if f.is_alive() => f.latest().is_some(),
            Some(_) => {
                // feed ขาด — เริ่มนับรอบ retry ใหม่
                self.feed = None;
                self.feed_retry = Some(Instant::now());
                false
            }
            None => false,
        }
    }

    fn tick_live(&mut self, ctx: &egui::Context) {
        let due = self.last_sample.is_none_or(|t| t.elapsed() >= self.interval);

        // ตำแหน่งแรก: ค่าสดจาก daemon ผ่าน pipe
        if self.tick_feed() && due {
            if let Some(f) = self.feed.as_ref()
                && let Some(snap) = f.latest() {
                    self.push_live(snap);
                    self.last_sample = Some(Instant::now());
                }
        // fallback: collector ในตัว
        } else if due
            && let Some(c) = self.collector.as_mut()
        {
            let snap = c.snapshot();
            self.push_live(snap);
            self.last_sample = Some(Instant::now());
        }
        // ให้จอวาดรอบถัดไปตามความถี่ sample — หน้า Live เต้นเอง อื่นวาดเมื่อมี interaction
        ctx.request_repaint_after(self.interval.min(Duration::from_secs(1)));
    }

    fn push_live(&mut self, s: Snapshot) {
        let (rx, tx) = s
            .nets
            .iter()
            .fold((0.0, 0.0), |(r, t), n| (r + n.rx_bps, t + n.tx_bps));
        self.live.push(RawPoint {
            ts: s.ts,
            cpu_total: Some(s.cpu_total as f64),
            cpu_temp: s.cpu_temp.map(|v| v as f64),
            mem_used: Some(s.mem_used as f64),
            mem_total: Some(s.mem_total as f64),
            swap_used: Some(s.swap_used as f64),
            net_rx: Some(rx),
            net_tx: Some(tx),
        });
        if self.live.len() > LIVE_POINTS {
            self.live.remove(0);
        }
        self.last_snap = Some(s);
    }

    // ---------- อ่านประวัติจาก SQLite ----------

    fn load_history(&mut self, now_ms: i64) {
        let Some(st) = self.storage.as_ref() else {
            return;
        };
        let range = self.hist_range;
        self.hist_err = None;

        let res = match hist_query(range, now_ms) {
            HistQuery::Raw { since, limit } => st
                .read_raw(since, limit)
                .map(|rows| self.hist = rows.iter().map(HistPoint::from_raw).collect()),
            HistQuery::Minute { since, limit } => st
                .read_1m(since, limit)
                .map(|rows| self.hist = rows.iter().map(HistPoint::from_bucket).collect()),
            HistQuery::Hour { since, limit } => st
                .read_1h(since, limit)
                .map(|rows| self.hist = rows.iter().map(HistPoint::from_bucket).collect()),
        };

        match res {
            Ok(()) => {
                self.hist_loaded_for = Some(range);
                self.hist_loaded_at = Some(Instant::now());
            }
            Err(msg) => {
                self.hist_err = Some(msg.to_string());
                self.hist_loaded_for = None;
            }
        }
    }

    /// ต้องโหลดซ้ำไหม: เปลี่ยนช่วง / ยังไม่โหลดช่วงนี้ / ข้อมูลเกิน 30 วิ
    fn should_reload_history(&self) -> bool {
        if self.storage.is_none() || self.tab != Tab::History {
            return false;
        }
        if self.hist_loaded_for != Some(self.hist_range) {
            return true;
        }
        self.hist_loaded_at
            .map(|t| t.elapsed() > Duration::from_secs(30))
            .unwrap_or(true)
    }
}

impl eframe::App for SystemMonitorApp {
    /// งานไม่วาดจอ: sample + อ่านประวัติ — รันระหว่างี่ window ซ่อนก็ยังเดิน
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.tick_live(ctx);
        if self.should_reload_history() {
            self.load_history(now_ms());
        }
    }

    /// วาดจอจริง — eframe 0.37 ให้ root Ui มาแล้ว ครอบด้วย panel เอง
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("header").show(ui, |panel| self.header(panel));

        egui::CentralPanel::default().show(ui, |panel| match self.tab {
            Tab::Live => self.ui_live(panel),
            Tab::History => self.ui_history(panel),
        });
    }
}

impl SystemMonitorApp {
    // ---------- แถบหัว: ค่าปัจจุบัน + ตัวเลือกหน้า ----------

    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.heading("System Monitor");
            if let Some(s) = &self.last_snap {
                ui.separator();
                ui.label(format!("CPU {:>5.1}%", s.cpu_total));
                if let Some(t) = s.cpu_temp {
                    ui.label(format!("{t:.0}°C"));
                }
                ui.label(format!(
                    "RAM {}/{} ({:.0}%)",
                    format_bytes(s.mem_used),
                    format_bytes(s.mem_total),
                    s.mem_usage_ratio() * 100.0
                ));
                let (rx, tx) = s
                    .nets
                    .iter()
                    .fold((0.0, 0.0), |(r, t), n| (r + n.rx_bps, t + n.tx_bps));
                ui.label(format!("↓ {} ↑ {}", format_rate(rx), format_rate(tx)));
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.selectable_value(&mut self.tab, Tab::History, "ประวัติ");
                ui.selectable_value(&mut self.tab, Tab::Live, "Live");
            });
        });
    }

    // ---------- หน้า Live ----------

    fn ui_live(&mut self, ui: &mut egui::Ui) {
        if let Some(err) = &self.collector_err {
            ui.colored_label(egui::Color32::RED, format!("Collector เปิดไม่ได้: {err}"));
            return;
        }
        if self.last_snap.is_none() {
            ui.label("กำลังอ่านค่าแรก…");
            return;
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("live_plots")
                .num_columns(2)
                .spacing([8.0, 8.0])
                .show(ui, |ui| {
                    self.plot_live_cpu(ui);
                    ui.end_row();
                    self.plot_live_ram(ui);
                    self.plot_live_net(ui);
                    ui.end_row();
                });

            self.ui_live_cores(ui);
            self.ui_live_gpus_disks(ui);
        });
    }

    fn plot_live_cpu(&self, ui: &mut egui::Ui) {
        let temp: Vec<[f64; 2]> = self
            .live
            .iter()
            .filter_map(|p| p.cpu_temp.map(|t| [p.ts as f64, t]))
            .collect();
        let cpu: Vec<[f64; 2]> = self
            .live
            .iter()
            .filter_map(|p| p.cpu_total.map(|v| [p.ts as f64, v]))
            .collect();

        Plot::new("live_cpu")
            .legend(Legend::default())
            .height(170.0)
            .default_y_bounds(0.0, 100.0)
            .x_axis_formatter(|mark: GridMark, _r| fmt_hms(mark.value as i64))
            .show(ui, |plot_ui| {
                plot_ui.line(
                    Line::new("CPU %", PlotPoints::from(cpu))
                        .color(egui::Color32::from_rgb(255, 160, 60)),
                );
                plot_ui.line(
                    Line::new("Temp °C", PlotPoints::from(temp))
                        .color(egui::Color32::from_rgb(120, 200, 255)),
                );
            });
    }

    fn plot_live_ram(&self, ui: &mut egui::Ui) {
        let gib: Vec<[f64; 2]> = self
            .live
            .iter()
            .filter_map(|p| p.mem_used.map(|v| [p.ts as f64, v / 1_073_741_824.0]))
            .collect();

        Plot::new("live_ram")
            .legend(Legend::default())
            .height(170.0)
            .y_axis_formatter(|mark: GridMark, _r| format!("{:.0} GiB", mark.value))
            .x_axis_formatter(|mark: GridMark, _r| fmt_hms(mark.value as i64))
            .show(ui, |plot_ui| {
                plot_ui.line(
                    Line::new("RAM ใช้ (GiB)", PlotPoints::from(gib))
                        .color(egui::Color32::from_rgb(110, 190, 110)),
                );
            });
    }

    fn plot_live_net(&self, ui: &mut egui::Ui) {
        let rx: Vec<[f64; 2]> = self
            .live
            .iter()
            .filter_map(|p| p.net_rx.map(|v| [p.ts as f64, v]))
            .collect();
        let tx: Vec<[f64; 2]> = self
            .live
            .iter()
            .filter_map(|p| p.net_tx.map(|v| [p.ts as f64, v]))
            .collect();

        Plot::new("live_net")
            .legend(Legend::default())
            .height(170.0)
            .y_axis_formatter(|mark: GridMark, _r| format_rate(mark.value))
            .x_axis_formatter(|mark: GridMark, _r| fmt_hms(mark.value as i64))
            .show(ui, |plot_ui| {
                plot_ui.line(
                    Line::new("รับ (rx)", PlotPoints::from(rx))
                        .color(egui::Color32::from_rgb(100, 140, 255)),
                );
                plot_ui.line(
                    Line::new("ส่ง (tx)", PlotPoints::from(tx))
                        .color(egui::Color32::from_rgb(255, 110, 110)),
                );
            });
    }

    fn ui_live_cores(&self, ui: &mut egui::Ui) {
        ui.separator();
        ui.label("ต่อ core");
        let cores: Vec<f32> = self
            .last_snap
            .as_ref()
            .map(|s| s.cpu_per_core.to_vec())
            .unwrap_or_default();
        let cols = cores.len().clamp(1, 16).div_ceil(4).clamp(1, 4);
        egui::Grid::new("cores")
            .num_columns(cols)
            .min_col_width(ui.available_width() / cols as f32 / 1.6)
            .show(ui, |ui| {
                for (i, v) in cores.iter().enumerate() {
                    if i > 0 && i % cols == 0 {
                        ui.end_row();
                    }
                    let pct = v.clamp(0.0, 100.0) / 100.0;
                    ui.add(
                        egui::ProgressBar::new(pct)
                            .show_percentage()
                            .text(format!("core {i}")),
                    );
                }
            });
    }

    fn ui_live_gpus_disks(&self, ui: &mut egui::Ui) {
        ui.separator();
        if let Some(s) = &self.last_snap {
            for g in &s.gpus {
                ui.horizontal_wrapped(|ui| {
                    ui.label(&g.name);
                    if let Some(usage) = g.usage_percent {
                        ui.add(
                            egui::ProgressBar::new((usage / 100.0).clamp(0.0, 1.0))
                                .show_percentage()
                                .text(format!("ใช้ {usage:.0}%")),
                        );
                    }
                    if let (Some(u), Some(t)) = (g.mem_used_mb, g.mem_total_mb) {
                        ui.add(
                            egui::ProgressBar::new((u as f32 / t.max(1) as f32).clamp(0.0, 1.0))
                                .show_percentage(),
                        );
                        ui.label(format!("VRAM {u} / {t} MiB"));
                    }
                    if let Some(t) = g.temp_c {
                        ui.label(format!("{t:.0}°C"));
                    }
                });
            }
            for d in &s.disks {
                ui.horizontal_wrapped(|ui| {
                    ui.label(&d.mount);
                    ui.add(
                        egui::ProgressBar::new(d.usage_ratio().clamp(0.0, 1.0))
                            .show_percentage()
                            .text(format!(
                                "{} / {}",
                                format_bytes(d.used_bytes),
                                format_bytes(d.total_bytes)
                            )),
                    );
                });
            }
        }
        ui.add_space(8.0);
    }

    // ---------- หน้าประวัติ ----------

    fn ui_history(&mut self, ui: &mut egui::Ui) {
        if let Some(err) = &self.storage_err {
            ui.colored_label(
                egui::Color32::RED,
                format!("เปิด SQLite ไม่ได้ ({err}) — รัน sysmon-daemon ก่อนเพื่อเก็บประวัติ"),
            );
            return;
        }

        ui.horizontal_wrapped(|ui| {
            for r in HistoryRange::all() {
                ui.selectable_value(&mut self.hist_range, r, r.label());
            }
            if ui.button("รีเฟรช").clicked() {
                self.hist_loaded_for = None;
            }
            ui.strong(self.hist_range.source_label());
        });

        if let Some(err) = &self.hist_err {
            ui.colored_label(egui::Color32::RED, format!("อ่านประวัติไม่สำเร็จ: {err}"));
            return;
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            self.plot_hist_cpu(ui);
            ui.add_space(8.0);
            self.plot_hist_ram(ui);
            ui.add_space(8.0);
            self.plot_hist_net(ui);
            ui.add_space(8.0);
        });
    }

    fn plot_hist_cpu(&self, ui: &mut egui::Ui) {
        let long = self.hist_range.long_ticks();
        if self.hist.is_empty() {
            ui.label("ยังไม่มีข้อมูลในช่วงนี้ — daemon ยังเก็บไม่พอ (ดู README)");
            return;
        }

        let mut band: Vec<[f64; 2]> = Vec::new();
        for p in &self.hist {
            if let Some(v) = p.cpu_min {
                band.push([p.ts as f64, v]);
            }
        }
        for p in self.hist.iter().rev() {
            if let Some(v) = p.cpu_max {
                band.push([p.ts as f64, v]);
            }
        }
        let band = downsample(band, HIST_MAX_POINTS);

        let cpu: Vec<[f64; 2]> = self
            .hist
            .iter()
            .filter_map(|p| p.cpu_avg.map(|v| [p.ts as f64, v]))
            .collect();
        let cpu = downsample(cpu, HIST_MAX_POINTS);

        let temps: Vec<[f64; 2]> = self
            .hist
            .iter()
            .filter_map(|p| p.cpu_temp.map(|v| [p.ts as f64, v]))
            .collect();
        let temps = downsample(temps, HIST_MAX_POINTS);

        Plot::new("hist_cpu")
            .legend(Legend::default())
            .height(200.0)
            .default_y_bounds(0.0, 100.0)
            .x_axis_formatter(move |mark: GridMark, _r| fmt_day_hm(mark.value as i64, long))
            .show(ui, |plot_ui| {
                plot_ui.polygon(
                    Polygon::new("min–max", PlotPoints::from(band))
                        .fill_color(egui::Color32::from_rgba_unmultiplied(60, 130, 240, 40)),
                );
                plot_ui.line(
                    Line::new("CPU เฉลี่ย %", PlotPoints::from(cpu))
                        .color(egui::Color32::from_rgb(255, 160, 60)),
                );
                plot_ui.line(
                    Line::new("Temp °C", PlotPoints::from(temps))
                        .color(egui::Color32::from_rgb(120, 200, 255)),
                );
            });
    }

    fn plot_hist_ram(&self, ui: &mut egui::Ui) {
        let long = self.hist_range.long_ticks();
        let mem: Vec<[f64; 2]> = self
            .hist
            .iter()
            .filter_map(|p| p.mem_used_gib.map(|v| [p.ts as f64, v]))
            .collect();
        let mem = downsample(mem, HIST_MAX_POINTS);
        if mem.is_empty() {
            return;
        }

        Plot::new("hist_ram")
            .legend(Legend::default())
            .height(170.0)
            .y_axis_formatter(|mark: GridMark, _r| format!("{:.1} GiB", mark.value))
            .x_axis_formatter(move |mark: GridMark, _r| fmt_day_hm(mark.value as i64, long))
            .show(ui, |plot_ui| {
                plot_ui.line(
                    Line::new("RAM ใช้ (GiB)", PlotPoints::from(mem))
                        .color(egui::Color32::from_rgb(110, 190, 110)),
                );
            });
    }

    fn plot_hist_net(&self, ui: &mut egui::Ui) {
        let long = self.hist_range.long_ticks();
        let rx: Vec<[f64; 2]> = self
            .hist
            .iter()
            .filter_map(|p| p.net_rx_bps.map(|v| [p.ts as f64, v]))
            .collect();
        let tx: Vec<[f64; 2]> = self
            .hist
            .iter()
            .filter_map(|p| p.net_tx_bps.map(|v| [p.ts as f64, v]))
            .collect();
        let rx = downsample(rx, HIST_MAX_POINTS);
        let tx = downsample(tx, HIST_MAX_POINTS);
        if rx.is_empty() {
            return;
        }

        Plot::new("hist_net")
            .legend(Legend::default())
            .height(170.0)
            .y_axis_formatter(|mark: GridMark, _r| format_rate(mark.value))
            .x_axis_formatter(move |mark: GridMark, _r| fmt_day_hm(mark.value as i64, long))
            .show(ui, |plot_ui| {
                plot_ui.line(
                    Line::new("รับ (rx)", PlotPoints::from(rx))
                        .color(egui::Color32::from_rgb(100, 140, 255)),
                );
                plot_ui.line(
                    Line::new("ส่ง (tx)", PlotPoints::from(tx))
                        .color(egui::Color32::from_rgb(255, 110, 110)),
                );
            });
    }
}

// ---------- ส่วนช่วย ----------

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// เวลาเครื่อง hh:mm:ss (ms → ผ่าน chrono Local)
fn fmt_hms(ms: i64) -> String {
    fmt_day_hm(ms, false)
}

fn fmt_day_hm(ms: i64, with_day: bool) -> String {
    use chrono::Local;
    let secs = ms.div_euclid(1000);
    let nanos = (ms.rem_euclid(1000).max(0) * 1_000_000) as u32;
    let pattern = if with_day { "%d/%m %H:%M" } else { "%H:%M:%S" };
    chrono::DateTime::from_timestamp(secs, nanos)
        .map(|d| d.with_timezone(&Local).format(pattern).to_string())
        .unwrap_or_default()
}

/// เหลือจุดไม่เกิน max: เว้นจุดแบบสม่ำเสมอ คงหัวท้ายไว้
fn downsample(pts: Vec<[f64; 2]>, max: usize) -> Vec<[f64; 2]> {
    if max == 0 || pts.len() <= max {
        return pts;
    }
    let n = pts.len();
    let step = n as f64 / max as f64;
    let mut out = Vec::with_capacity(max);
    for i in 0..max {
        out.push(pts[(i as f64 * step) as usize]);
    }
    let last = pts[n - 1];
    if out.last() != Some(&last) {
        out.push(last);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downsample_keeps_bounds_and_max() {
        let pts: Vec<[f64; 2]> = (0..100).map(|i| [i as f64, i as f64]).collect();
        let out = downsample(pts.clone(), 50);
        assert_eq!(out.len(), 51); // 50 จุด + หัวท้าย
        assert_eq!(out[0], pts[0]);
        assert_eq!(*out.last().unwrap(), [99.0, 99.0]);
    }

    #[test]
    fn downsample_passthrough_when_under_max() {
        let pts: Vec<[f64; 2]> = vec![[1.0, 1.0], [2.0, 2.0]];
        assert_eq!(downsample(pts.clone(), 10).len(), 2);
    }
}

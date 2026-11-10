//! MonitorLoop — เก็บค่า + เขียน DB แบบ batch + รายหรายงานสถานะ
//!
//! โหมด console พิมพ์ headline ทุก tick; โหมด tray เงียบ แล้วส่งข้อความสรุปกลับ
//! ให้ tray thread อัพเดต tooltip ผ่าน channel ทุก 2 วิ
//!
//! ออก: `quit` flag (จาก Ctrl+C หรือเมนู tray) → เขียนข้อมูลค้างลง DB ก่อนจบ

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::Result;

use sysmon_collector::Collector;
use sysmon_core::{Config, ProcessSort, ProcessStat, Snapshot};
use sysmon_storage::{hour_bucket, minute_bucket, Storage};

/// ช่วงส่งสรุปขึ้น tooltip
const STATUS_EVERY: Duration = Duration::from_secs(2);
/// ช่วงพิมพ์รายงาน measure (--measure)
const MEASURE_EVERY_TICKS: u64 = 60;

pub struct MonitorLoop {
    config: Config,
    top_n: Option<usize>,
    /// ไม่พิมพ์ headline (โหมด tray)
    quiet: bool,
    measure: bool,
    status_tx: Option<Sender<String>>,
}

impl MonitorLoop {
    pub fn new(
        config: Config,
        top_n: Option<usize>,
        quiet: bool,
        measure: bool,
        status_tx: Option<Sender<String>>,
    ) -> Self {
        Self {
            config,
            top_n,
            quiet,
            measure,
            status_tx,
        }
    }

    pub fn run(&mut self, quit: Arc<AtomicBool>) -> Result<()> {
        let config = &self.config;

        let mut storage: Option<Storage> = if config.storage.enabled {
            match Storage::open(&config.storage.db_path) {
                Ok(s) => {
                    eprintln!("[db] {}", config.storage.db_path);
                    Some(s)
                }
                Err(e) => {
                    eprintln!(
                        "[db] เปิด {} ไม่สำเร็จ ({e}) → ทำงานต่อโดยไม่เก็บประวัติ",
                        config.storage.db_path
                    );
                    None
                }
            }
        } else {
            None
        };

        eprintln!(
            "sysmon-daemon: interval={}ms disks={} nets={} temps={} top={}",
            config.sample_interval_ms,
            config.collector.monitor_disks,
            config.collector.monitor_networks,
            config.collector.monitor_temps,
            self.top_n.unwrap_or(0)
        );

        let mut collector = Collector::new(&config.collector)?;
        let interval = Duration::from_millis(config.sample_interval_ms.max(50));

        // ส่วนทำงานกับ db ตามแผน Phase 3
        let flush_every = Duration::from_secs(config.storage.flush_secs.max(1));
        let mut buffer: Vec<Snapshot> = Vec::new();
        let mut last_flush = Instant::now();
        let mut last_minute_done = 0i64;
        let mut last_hour_done = 0i64;
        let mut next_retention = Instant::now(); // ล้างของเก่าทันทีหลังเปิดครั้งแรก

        let mut measure = Measure::new(self.measure);
        let mut last_status = Instant::now() - STATUS_EVERY; // ส่งรอบแรกทันที

        loop {
            if quit.load(Ordering::Relaxed) {
                break;
            }
            let t0 = Instant::now();
            let snap = collector.snapshot();
            measure.snapshot(t0.elapsed());

            if let Some(st) = storage.as_mut() {
                buffer.push(snap.clone());

                if last_flush.elapsed() >= flush_every {
                    match st.insert_snapshots(&buffer) {
                        Ok(n) => {
                            measure.flushed(n);
                            eprintln!("[db] flush {n} แถว");
                        }
                        Err(e) => eprintln!("[db] เขียนไม่สำเร็จ: {e}"),
                    }
                    buffer.clear();
                    last_flush = Instant::now();

                    // rollup bucket ที่ "ปิดแล้ว" — idempotent จึงรันซ้ำได้ (กันข้าม bucket ตอนหยุดพัก)
                    let prev_min = minute_bucket(snap.ts) - 60_000;
                    if prev_min > last_minute_done {
                        if let Err(e) = st.rollup_minute_bucket(prev_min) {
                            eprintln!("[db] rollup นาทีไม่สำเร็จ: {e}");
                        }
                        last_minute_done = prev_min;
                    }
                    let prev_hour = hour_bucket(snap.ts) - 3_600_000;
                    if prev_hour > last_hour_done {
                        if let Err(e) = st.rollup_hour_bucket(prev_hour) {
                            eprintln!("[db] rollup ชั่วโมงไม่สำเร็จ: {e}");
                        }
                        last_hour_done = prev_hour;
                    }

                    // retention หนึ่งวันละครั้ง (plan) — ใช้ทุก 6 ชม. กัน process ยาว ๆ ข้ามวัน
                    if next_retention.elapsed() > Duration::from_secs(0) {
                        let now_ms = snap.ts;
                        if let Err(e) =
                            st.apply_retention(now_ms, config.storage.retention_raw_hours)
                        {
                            eprintln!("[db] retention ไม่สำเร็จ: {e}");
                        }
                        next_retention = Instant::now() + Duration::from_secs(6 * 3600);
                    }
                }
            }

            if !self.quiet {
                println!("{}", headline(&snap));
                if let Some(n) = self.top_n {
                    for (i, p) in collector.processes(n, ProcessSort::Cpu).iter().enumerate() {
                        println!("{}", process_row(i, p));
                    }
                }
            }

            // สรุปขึ้น tray tooltip ทุก 2 วิ
            if let Some(tx) = self.status_tx.as_ref()
                && last_status.elapsed() >= STATUS_EVERY {
                    if tx.send(tray_status(&snap)).is_err() {
                        // ผู้รับปิดไปแล้ว — หยุดส่ง (ยังเก็บข้อมูลต่อ)
                        self.status_tx = None;
                    } else {
                        last_status = Instant::now();
                    }
                }

            measure.tick();
            sleep(interval);
        }

        // ---- flush-before-exit: เขียนข้อมูลค้างก่อนปิด (แก้ data loss ของแผนแรก) ----
        if let Some(st) = storage.as_mut()
            && !buffer.is_empty() {
                match st.insert_snapshots(&buffer) {
                    Ok(n) => eprintln!("[exit] flush {n} แถวลง DB ก่อนปิด"),
                    Err(e) => eprintln!(
                        "[exit] เขียนข้อมูลค้างไม่สำเร็จ ({e}) — ข้อมูล {} แถวที่ค้างจะหาย",
                        buffer.len()
                    ),
                }
                // rollup bucket ปิดล่าสุดหนึ่งรอบปิดท้าย (idempotent)
                if let Some(last) = buffer.last() {
                    if let Err(e) = st.rollup_minute_bucket(minute_bucket(last.ts) - 60_000) {
                        eprintln!("[exit] rollup นาทีไม่สำเร็จ: {e}");
                    }
                    if let Err(e) = st.rollup_hour_bucket(hour_bucket(last.ts) - 3_600_000) {
                        eprintln!("[exit] rollup ชั่วโมงไม่สำเร็จ: {e}");
                    }
                }
                buffer.clear();
            }

        eprintln!("[daemon] ปิดแล้ว");
        Ok(())
    }
}

/// สรุปหนึ่งบรรทัดไว้ให้ tray tooltip (Windows tooltip แสดงทีละบรรทัด)
fn tray_status(s: &Snapshot) -> String {
    format!(
        "sysmon — CPU {:4.1}%  RAM {:.1}/{:.1} GiB",
        s.cpu_total,
        s.mem_used as f64 / 1_073_741_824.0,
        s.mem_total as f64 / 1_073_741_824.0
    )
}

/// สถิติ profiling เมื่อรันด้วย `--measure` — พิมพ์ทุก 60 ticks
struct Measure {
    enabled: bool,
    ticks: u64,
    snap_ms_total: f32,
    snap_ms_max: f32,
    snap_n: u32,
    rows_since_last_report: u64,
}

impl Measure {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            ticks: 0,
            snap_ms_total: 0.0,
            snap_ms_max: 0.0,
            snap_n: 0,
            rows_since_last_report: 0,
        }
    }

    fn snapshot(&mut self, d: Duration) {
        let ms = d.as_secs_f32() * 1000.0;
        self.snap_ms_total += ms;
        self.snap_ms_max = self.snap_ms_max.max(ms);
        self.snap_n += 1;
    }

    fn flushed(&mut self, n: usize) {
        self.rows_since_last_report += n as u64;
    }

    fn tick(&mut self) {
        if !self.enabled {
            return;
        }
        self.ticks += 1;
        if self.ticks.is_multiple_of(MEASURE_EVERY_TICKS) {
            let avg = self.snap_ms_total / self.snap_n.max(1) as f32;
            eprintln!(
                "[measure] ticks {}–{}: snapshot avg {:.1} ms / max {:.1} ms · flush +{} rows",
                self.ticks - MEASURE_EVERY_TICKS + 1,
                self.ticks,
                avg,
                self.snap_ms_max,
                self.rows_since_last_report
            );
            self.snap_ms_total = 0.0;
            self.snap_ms_max = 0.0;
            self.snap_n = 0;
            self.rows_since_last_report = 0;
        }
    }
}

/// บรรทัดเดียวอ่านเร็ว ๆ เหมือน Task Manager ย่อ
fn headline(s: &Snapshot) -> String {
    let mut parts: Vec<String> = vec![
        format!(
            "CPU {:5.1}%  {}",
            s.cpu_total,
            match s.cpu_temp {
                Some(t) => format!("{}°C", t),
                None => "--".into(),
            }
        ),
        format!(
            "RAM {}/{} ({:.0}%)",
            sysmon_core::format_bytes(s.mem_used),
            sysmon_core::format_bytes(s.mem_total),
            s.mem_usage_ratio() * 100.0
        ),
    ];

    if s.swap_used > 0 {
        parts.push(format!("SWAP {}", sysmon_core::format_bytes(s.swap_used)));
    }

    for d in &s.disks {
        parts.push(format!(
            "{} {}/{} ({:.0}%)",
            d.mount,
            sysmon_core::format_bytes(d.used_bytes),
            sysmon_core::format_bytes(d.total_bytes),
            d.usage_ratio() * 100.0
        ));
    }

    for g in &s.gpus {
        parts.push(format!(
            "{}  {}%  {} MiB",
            g.name,
            g.usage_percent
                .map(|u| format!("{:.0}", u))
                .unwrap_or("--".into()),
            g.mem_used_mb.unwrap_or(0)
        ));
    }

    // สรุป network เป็นตัวเดียว (ผลรวมทุก interface) — ต่อ interface ดูใน tui
    let (rx, tx) = s
        .nets
        .iter()
        .fold((0.0, 0.0), |(r, t), n| (r + n.rx_bps, t + n.tx_bps));
    if rx > 0.0 || tx > 0.0 {
        parts.push(format!(
            "NET rx {} tx {}",
            sysmon_core::format_rate(rx),
            sysmon_core::format_rate(tx)
        ));
    }

    parts.join(" | ")
}

fn process_row(i: usize, p: &ProcessStat) -> String {
    format!(
        " {:>3}. {:<34} {:5.1}%  {}",
        i + 1,
        p.name,
        p.cpu_percent,
        sysmon_core::format_bytes(p.mem_bytes)
    )
}

#[cfg(test)]
mod tests {
    use super::MonitorLoop;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    use sysmon_core::Config;
    use sysmon_storage::Storage;

    /// flush-before-exit: quit flag → ข้อมูลค้างใน buffer ต้องลง DB แม้ไม่ถึงรอบ flush
    #[test]
    fn quit_flag_flushes_pending_buffer() {
        let dir = std::env::temp_dir().join(format!("sysmon-daemon-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let mut cfg = Config::default();
        cfg.sample_interval_ms = 100;
        cfg.storage.flush_secs = 3600; // กัน flush ตามรอบปกติ — ให้แถวมาจากบันทึกของตอนออกแท้ ๆ
        cfg.storage.db_path = dir.join("exit.db").to_string_lossy().to_string();

        let quit = Arc::new(AtomicBool::new(false));
        let handle = {
            let q = Arc::clone(&quit);
            let cfg2 = cfg.clone();
            thread::spawn(move || {
                MonitorLoop::new(cfg2, None, true, false, None).run(q).unwrap()
            })
        };
        thread::sleep(Duration::from_millis(1500)); // เก็บได้ 1-2 sample
        quit.store(true, std::sync::atomic::Ordering::Relaxed);
        handle.join().unwrap();

        let rows = Storage::open(&cfg.storage.db_path)
            .unwrap()
            .read_raw(0, 100)
            .unwrap()
            .len();
        assert!(rows >= 1, "quit แล้วต้องเขียน buffer {rows} แถวลง DB ≥1");

        drop(std::fs::remove_dir_all(&dir));
    }
}

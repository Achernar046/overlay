//! sysmon-daemon — เก็บค่าและเขียนลง SQLite แบบ batch (Phase 1-3)
//! - พิมพ์ headline ออก stdout ทุก tick
//! - `--top N` พิมพ์ top-N process ต่อท้าย
//! - Phase 3: เขียนลง sysmon.db ทุก `flush_secs`, rollup 1m/1h, retention
//!   (IPC ให้ ui/overlay มาจาก Phase ที่ตามมา)

use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::Result;

use sysmon_collector::Collector;
use sysmon_core::{Config, ProcessSort, ProcessStat, Snapshot};
use sysmon_storage::{hour_bucket, minute_bucket, Storage};

fn main() -> Result<()> {
    let mut warn = |msg: &str| eprintln!("[config] {msg}");
    let config = Config::load_or_default("sysmon.toml", &mut warn);

    // CLI แบบง่าย: `sysmon-daemon --top 10`
    let args: Vec<String> = std::env::args().collect();
    let top_n: Option<usize> = args
        .iter()
        .position(|a| a == "--top")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok());

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
        top_n.unwrap_or(0)
    );

    let mut collector = Collector::new(&config.collector)?;
    let interval = Duration::from_millis(config.sample_interval_ms.max(50));

    // กำหนดเวลาการทำงานของ db:
    let flush_every = Duration::from_secs(config.storage.flush_secs.max(1));
    let mut buffer: Vec<Snapshot> = Vec::new();
    let mut last_flush = Instant::now();
    let mut last_minute_done = 0i64;
    let mut last_hour_done = 0i64;
    let mut next_retention = Instant::now(); // ล้างของเก่าทันทีหลังเปิดโปรแกรมครั้งแรก

    loop {
        let snap = collector.snapshot();

        if let Some(st) = storage.as_mut() {
            buffer.push(snap.clone());

            if last_flush.elapsed() >= flush_every {
                match st.insert_snapshots(&buffer) {
                    Ok(n) => eprintln!("[db] flush {n} แถว"),
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

                // retention หนึ่งวันละครั้ง (plan) — ใช้ทุก 6 ชม. กัน process ยาวๆ ข้ามวัน
                if next_retention.elapsed() > Duration::from_secs(0) {
                    let now_ms = snap.ts;
                    if let Err(e) = st.apply_retention(now_ms, config.storage.retention_raw_hours) {
                        eprintln!("[db] retention ไม่สำเร็จ: {e}");
                    }
                    next_retention = Instant::now() + Duration::from_secs(6 * 3600);
                }
            }
        }

        println!("{}", headline(&snap));
        if let Some(n) = top_n {
            for (i, p) in collector.processes(n, ProcessSort::Cpu).iter().enumerate() {
                println!("{}", process_row(i, p));
            }
        }
        sleep(interval);
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
            g.usage_percent.map(|u| format!("{:.0}", u)).unwrap_or("--".into()),
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

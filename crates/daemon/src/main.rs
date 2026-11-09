//! sysmon-daemon — Phase 1: ทดสอบการ sample โดยพิมพ์ headline ออก stdout
//! Phase 3 จะเพิ่ม: SQLite batch writer + IPC ให้ ui/overlay

use std::thread::sleep;
use std::time::Duration;

use anyhow::Result;

use sysmon_collector::Collector;
use sysmon_core::{Config, ProcessSort, ProcessStat, Snapshot};

fn main() -> Result<()> {
    let mut warn = |msg: &str| eprintln!("[config] {msg}");
    let config = Config::load_or_default("sysmon.toml", &mut warn);
    // CLI แบบง่าย: `sysmon-daemon --top 10` พิมพ์ top process ต่อท้าย headline ทุก tick
    let args: Vec<String> = std::env::args().collect();
    let top_n: Option<usize> = args
        .iter()
        .position(|a| a == "--top")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok());
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

    loop {
        let snap = collector.snapshot();
        println!("{}", headline(&snap));
        if let Some(n) = top_n {
            for (i, p) in collector.processes(n, ProcessSort::Cpu).iter().enumerate() {
                println!("{}", process_row(i, p));
            }
        }
        sleep(interval);
    }
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

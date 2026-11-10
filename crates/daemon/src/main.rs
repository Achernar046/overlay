//! sysmon-daemon — เก็บค่าและเขียนลง SQLite แบบ batch (Phase 1-3, 5-6)
//! - โหมดปกติ: พิมพ์ headline ออก stdout ทุก tick (`--top N` เพิ่ม top-N process)
//! - `--tray`: ทำงานจาก system tray (Windows) — ซ่อน console, เมนูเปิด ui/overlay,
//!   toggle autostart, ออกอย่างปลอดภัย; console จะถูกซ่อนเมื่อเริ่ม
//! - `--autostart on|off|show`: คุม register Run key ทีละคำสั่ง
//! - `--print-config`: พิมพ์ไฟล์ sysmon.toml ตัวอย่าง (ค่า default)
//! - Ctrl+C / เมนู "ออก" → flush ข้อมูลใน buffer ลง DB ก่อนปิด (แก้ data loss ของ Phase 3)
//!
//! '"'Phase' 3: เขียนลง sysmon.db ทุก `flush_secs`, rollup 1m/1h, retention
//!   (IPC ให้ ui/overlay มาจาก Phase ที่ตามมา)

mod autostart;
mod monitor;
#[cfg(windows)]
mod tray;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;

use sysmon_core::Config;

use crate::monitor::MonitorLoop;

fn main() -> Result<()> {
    let args = parse_args();

    if args.help {
        print_usage();
        return Ok(());
    }
    if args.print_config {
        print!("{}", Config::example_toml());
        return Ok(());
    }

    // คำสั่ง autostart เป็น one-shot: ทำแล้วจบ ไม่ต้องรัน monitor
    if let Some(mode) = &args.autostart {
        #[cfg(windows)]
        {
            autostart::handle(mode)?;
            return Ok(());
        }
        #[cfg(not(windows))]
        {
            let _ = mode;
            eprintln!("--autostart รองรับเฉพาะ Windows");
            return Ok(());
        }
    }

    let mut warn = |msg: &str| eprintln!("[config] {msg}");
    let config = Config::load_or_default("sysmon.toml", &mut warn);

    // สัญญาณออก: Ctrl+C (ทุกโหมด) หรือเมนู "ออก" ใน tray
    let quit = Arc::new(AtomicBool::new(false));
    {
        let q = Arc::clone(&quit);
        match ctrlc::set_handler(move || {
            if !q.swap(true, Ordering::Relaxed) {
                eprintln!();
                eprintln!("[daemon] ได้รับสัญญาณออก — ข้อมูลที่ยังใน buffer จะถูกเขียนลง DB ก่อนปิด");
            }
        })
        {
            Ok(()) => {}
            Err(e) => eprintln!("[daemon] ตั้ง Ctrl+C handler ไม่ได้ ({e}) — ถ้ากด Ctrl+C ข้อมูลใน buffer จะไม่ถูกเขียน"),
        }
    }

    if args.tray {
        #[cfg(windows)]
        {
            tray::run(config, quit, args.measure)?;
            return Ok(());
        }
        #[cfg(not(windows))]
        {
            let _ = (config, &quit, args.measure);
            eprintln!("[daemon] --tray รองรับเฉพาะ Windows — ทำงานโหมด console แทน");
            return Ok(());
        }
    }

    MonitorLoop::new(config, args.top_n, false, args.measure, None).run(quit)
}

struct Args {
    help: bool,
    top_n: Option<usize>,
    tray: bool,
    measure: bool,
    print_config: bool,
    autostart: Option<String>,
}

fn parse_args() -> Args {
    let argv: Vec<String> = std::env::args().collect();
    let mut args = Args {
        help: false,
        top_n: None,
        tray: false,
        measure: false,
        print_config: false,
        autostart: None,
    };
    let mut it = argv.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--top" => {
                args.top_n = it.next().and_then(|v| v.parse().ok());
            }
            "--autostart" => {
                args.autostart = it.next().map(|v| v.to_ascii_lowercase());
            }
            "--tray" => args.tray = true,
            "--measure" => args.measure = true,
            "--print-config" => args.print_config = true,
            "-h" | "--help" => args.help = true,
            _ => {}
        }
    }
    args
}

fn print_usage() {
    eprintln!(
        "sysmon-daemon — background collector + SQLite writer

ใช้งาน: cargo run -p sysmon-daemon [options]

options:
  --top N        พิมพ์ top-N process ต่อท้าย headline (sort ตาม CPU)
  --tray         ทำงานจาก system tray โดยไม่จำเป็นต้องพิมพ์ stdout เมื่อรัน daemon ปกติ
                 (Windows; แนะนำเปิดใหม่ด้วย start ซ่อน console)
  --autostart mode  เปิด/ปิดตั้งให้เปิดเองตอนล็อกอิน: on | off | show (Windows)
  --measure      พิมพ์สถิติเวลาต่อ 60 ticks (snapshot ms / flush rows) เพื่อ profiling
  --print-config พิมพ์ sysmon.toml ตัวอย่าง (ค่า default) ไป stdout
  -h, --help     แสดงวิธีใช้นี้"
    );
}

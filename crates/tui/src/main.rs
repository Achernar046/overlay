//! sysmon-tui — dashboard ใน terminal สำหรับทดสอบ collector (Phase 1+2)
//! q / Esc = ออก, Tab = สลับหน้า System ↔ Processes, m = สลับ sort ในหน้า Processes

use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Gauge, Paragraph, Row, Sparkline, Table};
use ratatui::Frame;

use sysmon_collector::Collector;
use sysmon_core::{
    format_bytes, format_rate, Config, ProcessSort, ProcessStat, ProcessSort as Sort, Snapshot,
};

const HIST_LEN: usize = 120;
const PROC_INTERVAL: Duration = Duration::from_secs(2); // plan: refresh process list ทุก 2-3 วิ
const PROC_TOP_N: usize = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    System,
    Processes,
}

fn main() -> Result<()> {
    let mut stderr = |msg: &str| eprintln!("[config] {msg}");
    let config = Config::load_or_default("sysmon.toml", &mut stderr);

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &config);
    ratatui::restore();
    result
}

fn run(terminal: &mut ratatui::DefaultTerminal, config: &Config) -> Result<()> {
    let mut collector = Collector::new(&config.collector)?;
    let interval = Duration::from_millis(config.sample_interval_ms.max(50));
    let mut cpu_hist: Vec<f64> = Vec::new();
    let mut last_sample = Instant::now() - interval;

    let mut view = View::System;
    let mut sort = ProcessSort::Cpu;
    let mut procs: Vec<ProcessStat> = Vec::new();
    let mut last_proc = Instant::now() - PROC_INTERVAL;

    loop {
        while last_sample.elapsed() < interval {
            if event::poll(interval)?
                && let Event::Key(k) = event::read()?
                    && k.kind == KeyEventKind::Press
                {
                    match k.code {
                        KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                        KeyCode::Tab => {
                            view = match view {
                                View::System => View::Processes,
                                View::Processes => View::System,
                            };
                            // ออกจากหน้า process แล้วไม่ refresh process list (ประหยัด CPU)
                            if view == View::Processes {
                                last_proc -= PROC_INTERVAL; // บังคับ sample ทันที
                            }
                        }
                        KeyCode::Char('m') if view == View::Processes => {
                            sort = match sort {
                                ProcessSort::Cpu => ProcessSort::Memory,
                                ProcessSort::Memory => ProcessSort::DiskIo,
                                ProcessSort::DiskIo => ProcessSort::Cpu,
                            };
                            last_proc -= PROC_INTERVAL;
                        }
                        _ => {}
                    }
                }
        }

        let snap = collector.snapshot();
        cpu_hist.push(snap.cpu_total.clamp(0.0, 100.0) as f64);
        if cpu_hist.len() > HIST_LEN {
            cpu_hist.remove(0);
        }

        if view == View::Processes && last_proc.elapsed() >= PROC_INTERVAL {
            procs = collector.processes(PROC_TOP_N, sort).into_vec();
            last_proc = Instant::now();
        }

        let this_view = view;
        terminal.draw(|f| draw(f, &snap, &cpu_hist, this_view, sort, &procs))?;
        last_sample = Instant::now();
    }
}

fn draw(
    f: &mut Frame,
    s: &Snapshot,
    hist: &[f64],
    view: View,
    sort: Sort,
    procs: &[ProcessStat],
) {
    if view == View::Processes {
        draw_processes(f, sort, procs);
        return;
    }

    let disks_h = s.disks.len() as u16 + 2;
    let gpus_h = s.gpus.len() as u16 + 2;
    let nets_h = s.nets.len() as u16 + 2;

    let [header, cpu_area, ram_area, disk_area, gpu_area, net_area, spark_area] =
        Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(6),
            Constraint::Length(3),
            Constraint::Length(disks_h),
            Constraint::Length(gpus_h),
            Constraint::Length(nets_h),
            Constraint::Length(3),
        ])
        .areas(f.area());

    // ── header ──
    let temp = match s.cpu_temp {
        Some(t) => format!("{t:.0}°C"),
        None => "--".into(),
    };
    f.render_widget(
        Paragraph::new(Line::from(format!(
            "sysmon  CPU {:5.1}% ({temp})   {} elapsed   Tab = processes, q = ออก",
            s.cpu_total,
            hist.len(),
        ))),
        header,
    );

    draw_cpu_cores(f, s, cpu_area);

    // ── RAM ──
    let ram_ratio = s.mem_usage_ratio().clamp(0.0, 1.0) as f64;
    f.render_widget(
        Gauge::default()
            .ratio(ram_ratio)
            .label(format!(
                "RAM {} / {}",
                format_bytes(s.mem_used),
                format_bytes(s.mem_total)
            ))
            .style(Style::new().fg(Color::Yellow))
            .block(Block::bordered().title("Memory")),
        ram_area,
    );

    // ── Disks ──
    let disk_lines = s
        .disks
        .iter()
        .map(|d| {
            Line::from(format!(
                "{:12}  {} / {}  ({:.0}%)",
                d.mount,
                format_bytes(d.used_bytes),
                format_bytes(d.total_bytes),
                d.usage_ratio() * 100.0
            ))
        })
        .collect::<Vec<_>>();
    f.render_widget(
        Paragraph::new(disk_lines).block(Block::bordered().title("Disks")),
        disk_area,
    );

    // ── GPUs ──
    let gpu_lines = s
        .gpus
        .iter()
        .map(|g| {
            let temp = match g.temp_c {
                Some(t) => format!("{t:.0}°C"),
                None => "--".into(),
            };
            Line::from(format!(
                "{:30}  {:4}%  {} / {} MiB  {}",
                g.name,
                g.usage_percent.unwrap_or(0.0),
                g.mem_used_mb.unwrap_or(0),
                g.mem_total_mb.unwrap_or(0),
                temp,
            ))
        })
        .collect::<Vec<_>>();
    f.render_widget(
        Paragraph::new(gpu_lines).block(Block::bordered().title("GPU")),
        gpu_area,
    );

    // ── Network ──
    let rows = s
        .nets
        .iter()
        .map(|n| {
            Row::new(vec![
                n.name.clone(),
                format!("↓{}", format_rate(n.rx_bps)),
                format!("↑{}", format_rate(n.tx_bps)),
            ])
        })
        .collect::<Vec<_>>();
    f.render_widget(
        Table::new(
            rows,
            [
                Constraint::Percentage(40),
                Constraint::Percentage(30),
                Constraint::Percentage(30),
            ],
        )
        .header(Row::new(["interface", "RX", "TX"]).style(Style::new().fg(Color::Cyan)))
        .block(Block::bordered().title("Network")),
        net_area,
    );

    // ── CPU history ──
    f.render_widget(
        Sparkline::default()
            .data(hist.iter().map(|v| *v as u64))
            .style(Style::new().fg(Color::Cyan))
            .block(Block::bordered().title("CPU % (ล่าสุด)")),
        spark_area,
    );
}

fn draw_processes(f: &mut Frame, sort: Sort, procs: &[ProcessStat]) {
    let sort_title = match sort {
        Sort::Cpu => "CPU",
        Sort::Memory => "MEM",
        Sort::DiskIo => "DISK",
    };
    let rows = procs
        .iter()
        .map(|p| {
            Row::new(vec![
                p.pid.to_string(),
                p.name.clone(),
                format!("{:.1}%", p.cpu_percent),
                format_bytes(p.mem_bytes),
                p.io_bps.map(|v| format_rate(v as f64)).unwrap_or_else(|| "-".into()),
            ])
        })
        .collect::<Vec<_>>();
    f.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(8),
                Constraint::Min(30),
                Constraint::Length(8),
                Constraint::Length(12),
                Constraint::Length(10),
            ],
        )
        .header(Row::new(["pid", "name", "CPU", "MEM", "DISK I/O"]).style(Style::new().fg(Color::Cyan)))
        .block(
            Block::bordered()
                .title(format!("Processes — top {} ตาม {sort_title}  (Tab = กลับ, m = สลับ sort)", PROC_TOP_N)),
        ),
        f.area(),
    );
}

/// วาด gauge ต่อ core เป็นตาราง 4 คอลัมน์
fn draw_cpu_cores(f: &mut Frame, s: &Snapshot, area: ratatui::layout::Rect) {
    let cores = s.cpu_per_core.len();
    if cores == 0 {
        return;
    }
    const COLS: usize = 4;
    let rows = cores.div_ceil(COLS) as u16;

    let row_chunks = Layout::vertical(vec![Constraint::Ratio(1, rows as u32); rows as usize])
        .split(area);
    for (i, usage) in s.cpu_per_core.iter().enumerate() {
        let row = &row_chunks[i / COLS];
        let cols = Layout::horizontal(vec![Constraint::Ratio(1, COLS as u32); COLS]).split(*row);
        let gauge = Gauge::default()
            .ratio(((usage / 100.0) as f64).clamp(0.0, 1.0))
            .label(format!("{i}: {usage:.0}%"))
            .style(Style::new().fg(Color::Cyan))
            .block(Block::default());
        f.render_widget(gauge, cols[i % COLS]);
    }
}

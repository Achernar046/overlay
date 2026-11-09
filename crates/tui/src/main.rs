//! sysmon-tui — dashboard ใน terminal สำหรับทดสอบ collector (Phase 1)
//! q / Esc เพื่อออก

use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Gauge, Paragraph, Row, Sparkline, Table};
use ratatui::Frame;

use sysmon_collector::Collector;
use sysmon_core::{format_bytes, format_rate, Config, Snapshot};

const HIST_LEN: usize = 120;

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

    loop {
        while last_sample.elapsed() < interval {
            if event::poll(interval)?
                && let Event::Key(k) = event::read()?
                    && k.kind == KeyEventKind::Press
                        && matches!(k.code, KeyCode::Char('q') | KeyCode::Esc)
                    {
                        return Ok(());
                    }
        }

        let snap = collector.snapshot();
        cpu_hist.push(snap.cpu_total.clamp(0.0, 100.0) as f64);
        if cpu_hist.len() > HIST_LEN {
            cpu_hist.remove(0);
        }

        terminal.draw(|f| draw(f, &snap, &cpu_hist))?;
        last_sample = Instant::now();
    }
}

fn draw(f: &mut Frame, s: &Snapshot, hist: &[f64]) {
    let disks_h = s.disks.len() as u16 + 2;
    let nets_h = s.nets.len() as u16 + 2;

    let [header, cpu_area, ram_area, disk_area, net_area, spark_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(6),
        Constraint::Length(3),
        Constraint::Length(disks_h),
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
            "sysmon  CPU {:5.1}% ({temp})   {} elapsed{}",
            s.cpu_total,
            hist.len(),
            "   q = ออก"
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
        .header(
            Row::new(["interface", "RX", "TX"])
                .style(Style::new().fg(Color::Cyan)),
        )
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

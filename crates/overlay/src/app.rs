//! OverlayApp — วาดสถิติสั้น ๆ บนหน้าต่างโปร่งแสง + ควบคุมด้วย hotkey กลางระบบ
//!
//! วาดด้วย painter ตรง ๆ (ไม่พึ่ง widget layout) — คุมสี/ความโปร่งเองทั้งหมด

use std::time::{Duration, Instant};

use eframe::egui;
use eframe::egui::{Align2, Color32, FontId, Rect, Sense, Vec2};
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

use sysmon_collector::Collector;
use sysmon_core::ipc::SnapshotFeed;
use sysmon_core::{format_bytes, format_rate, CollectorConfig, Snapshot};

/// ขนาดพื้นฐาน (point) คูณด้วย `scale` ตอนวาด — window ขนาดตรงตาม scale ที่ set ไว้ตอนสร้าง
const HEADER_H: f32 = 20.0;
const ROW_H: f32 = 24.0;
const ROW_GAP: f32 = 2.0;
const PAD: f32 = 8.0;

/// ปุ่มลัดที่ register ไว้
pub struct Keys {
    pub toggle: HotKey,
    pub interact: HotKey,
}

pub struct OverlayApp {
    /// ค่าสดจาก daemon ผ่าน pipe (ถ้าต่อได้) — ได้ก่อน ไม่ sample เอง
    feed: Option<SnapshotFeed>,
    feed_retry: Option<Instant>,
    ipc_name: Option<String>,
    collector: Option<Collector>,
    collector_err: Option<String>,
    snapshot: Option<Snapshot>,
    last_sample: Option<Instant>,
    interval: Duration,
    light_theme: bool,
    theme_set: Option<bool>,
    scale: f32,
    visible: bool,
    /// โหมด interactive = รับเมาส์ได้ (ลาก/ปิดหน้าต่าง) — ไม่ interactive = click-through
    interactive: bool,
    pass_through_sent: bool,
    keys: Keys,
    /// ต้องมีชีวิตอยู่ตลอด ไม่เช่นนั้น hotkey จะถูก unregister
    manager: Option<GlobalHotKeyManager>,
    /// ตำแหน่งหน้าต่างล่าสุด (monitor space, points) — บันทึกผ่าน eframe persistence
    saved_pos: Option<egui::Pos2>,
}

impl OverlayApp {
    // constructor รับค่าเตรียมตั้งจาก main ครบชุด — รับเยอะเป็นธรรมชาติ
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        _cc: &eframe::CreationContext<'_>,
        collector_config: CollectorConfig,
        sample_interval_ms: u64,
        light_theme: bool,
        scale: f32,
        keys: Keys,
        manager: Option<GlobalHotKeyManager>,
        ipc_name: Option<String>,
    ) -> Self {
        let (collector, collector_err) = match Collector::new(&collector_config) {
            Ok(c) => (Some(c), None),
            Err(e) => (None, Some(e.to_string())),
        };
        // ตำแหน่งที่ผู้ใช้ลากไว้ครั้งก่อน — restore ทับค่า margin จาก config
        let saved_pos = _cc
            .storage
            .as_ref()
            .and_then(|st| st.get_string("overlay_pos"))
            .and_then(|s| {
                let (x, y) = s.split_once(',')?;
                let x = x.trim().parse::<f32>().ok()?;
                let y = y.trim().parse::<f32>().ok()?;
                Some(egui::Pos2::new(x, y))
            });
        Self {
            feed: None,
            feed_retry: Some(Instant::now()),
            ipc_name,
            collector,
            collector_err,
            snapshot: None,
            last_sample: None,
            interval: Duration::from_millis(sample_interval_ms),
            light_theme,
            theme_set: None,
            scale,
            visible: true,
            interactive: false,
            pass_through_sent: false,
            keys,
            manager,
            saved_pos,
        }
    }

    // ---------- logic-cycle helpers ----------

    /// เติม visuals ครั้งแรก + ส่งสถานะ click-through ครั้งแรก
    fn apply_initial_state(&mut self, ctx: &egui::Context) {
        if self.theme_set != Some(self.light_theme) {
            if self.light_theme {
                ctx.set_visuals(egui::Visuals::light());
            } else {
                ctx.set_visuals(egui::Visuals::dark());
            }
            self.theme_set = Some(self.light_theme);
        }
        // เริ่มต้น click-through กันบังเกม — กด interact hotkey เพื่อลาก/ปิด
        if !self.pass_through_sent {
            ctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(!self.interactive));
            self.pass_through_sent = true;
        }
        // ตำแหน่งที่บันทึกไว้ครั้งก่อน → ย้ายไปครั้งเดียวตอนเปิด (ทับค่า margin จาก options)
        if let Some(pos) = self.saved_pos.take() {
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
        }
    }

    /// อัพเดตตำแหน่งล่าสุดของหน้าต่าง (เรียกทุกเฟรมที่วาดจอ) เพื่อเก็บลง storage
    fn track_position(&mut self, ctx: &egui::Context) {
        let pos = ctx.input(|i| i.viewport().outer_rect.map(|r| r.min));
        match (pos, self.saved_pos) {
            (Some(p), Some(old)) if (p.x - old.x).abs() > 0.5 || (p.y - old.y).abs() > 0.5 => {
                self.saved_pos = Some(p);
            }
            (Some(p), None) => {
                self.saved_pos = Some(p);
            }
            _ => {}
        }
    }

    /// อ่านเหตุการณ์ hotkey ทุกเฟรม (logic รันแม้ window ซ่อน → toggle ได้จริง)
    fn poll_hotkeys(&mut self, ctx: &egui::Context) {
        if self.manager.is_none() {
            return;
        }
        let rx = GlobalHotKeyEvent::receiver();
        while let Ok(ev) = rx.try_recv() {
            // รับแค่ตอนกด — สั่นตอนปล่อยปุ่มไม่ต้องสน
            if ev.state != HotKeyState::Pressed {
                continue;
            }
            if ev.id == self.keys.toggle.id() {
                self.visible = !self.visible;
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(self.visible));
            } else if ev.id == self.keys.interact.id() {
                self.interactive = !self.interactive;
                ctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(!self.interactive));
            }
        }
    }

    /// sample ทุก interval แม้ซ่อนหน้าต่าง — พร้อมขึ้นจอเป๊ะทันที
    fn tick_sample(&mut self, ctx: &egui::Context) {
        let due = self.last_sample.is_none_or(|t| t.elapsed() >= self.interval);

        // ตำแหน่งแรก: ค่าสดจาก daemon ผ่าน pipe (ลองต่อใหม่ทุก 10 วิ ถ้ายังไม่ได้)
        let mut from_ipc = false;
        if self.feed.is_none()
            && let Some(t) = self.feed_retry
                && t.elapsed() >= Duration::from_secs(10) {
                    self.feed = self
                        .ipc_name
                        .as_deref()
                        .map(SnapshotFeed::connect)
                        .and_then(|r| r.ok());
                    self.feed_retry = Some(Instant::now());
                }
        match self.feed.as_ref() {
            Some(f) if f.is_alive() => {
                if due
                    && let Some(snap) = f.latest()
                {
                    self.snapshot = Some(snap);
                    self.last_sample = Some(Instant::now());
                    from_ipc = true;
                }
            }
            Some(_) => {
                // feed ขาด — จะลองใหม่ในอีก 10 วิ
                self.feed = None;
                self.feed_retry = Some(Instant::now());
            }
            None => {}
        }

        // fallback: collector ในตัว
        if !from_ipc
            && due
            && let Some(c) = self.collector.as_mut()
        {
            let snap = c.snapshot();
            self.snapshot = Some(snap);
            self.last_sample = Some(Instant::now());
        }
        // เต้นช้าลงเมื่อซ่อนตัว (ยังต้องเรียกครั้งนึงเพื่อรับ hotkey event ถัดไป)
        let hibernate = if self.visible {
            self.interval.min(Duration::from_millis(250))
        } else {
            self.interval
        };
        ctx.request_repaint_after(hibernate);
    }

    // ---------- ข้อมูลแถว metric ----------

    /// (ชื่อ, ข้อความ, ratio สำหรับ bar 0.0–1.0) — สูงสุด 4 แถว
    fn metric_rows(&self) -> Vec<(String, String, Option<f32>)> {
        let Some(snap) = self.snapshot.as_ref() else {
            return vec![
                ("CPU".into(), "…".into(), None),
                ("RAM".into(), "…".into(), None),
                ("GPU".into(), "…".into(), None),
                ("NET".into(), "…".into(), None),
            ];
        };
        let mut rows: Vec<(String, String, Option<f32>)> = Vec::with_capacity(4);

        let temp = snap
            .cpu_temp
            .map(|t| format!(" {:.0}°C", t))
            .unwrap_or_default();
        rows.push((
            "CPU".into(),
            format!("{:5.1}%{}", snap.cpu_total, temp),
            Some(snap.cpu_total / 100.0),
        ));

        rows.push((
            "RAM".into(),
            format!(
                "{} / {}",
                format_bytes(snap.mem_used),
                format_bytes(snap.mem_total)
            ),
            Some(snap.mem_usage_ratio()),
        ));

        if let Some(g) = snap.gpus.first() {
            rows.push((
                "GPU".into(),
                format!(
                    "{:.0}%{}",
                    g.usage_percent.unwrap_or(0.0),
                    g.mem_used_mb
                        .map(|m| format!(" {} MiB", m))
                        .unwrap_or_default()
                ),
                Some(g.usage_percent.unwrap_or(0.0) / 100.0),
            ));
        }

        let (rx, tx) = snap
            .nets
            .iter()
            .fold((0.0, 0.0), |(r, t), n| (r + n.rx_bps, t + n.tx_bps));
        rows.push((
            "NET".into(),
            format!("↓{} ↑{}", format_rate(rx), format_rate(tx)),
            None,
        ));

        rows
    }

    /// สี bar ตามค่า — เขียว < 60% / เหลือง < 85% / แดง
    fn bar_color(ratio: f32) -> Color32 {
        if ratio >= 0.85 {
            Color32::from_rgb(229, 72, 80)
        } else if ratio >= 0.6 {
            Color32::from_rgb(232, 181, 67)
        } else {
            Color32::from_rgb(84, 196, 120)
        }
    }
}

impl eframe::App for OverlayApp {
    /// งานไม่เกี่ยวกับการวาด: sample + hotkey — รันแม้หน้าต่างถูกซ่อน (toggle ได้จริง)
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.apply_initial_state(ctx);
        self.poll_hotkeys(ctx);
        self.tick_sample(ctx);
        self.track_position(ctx);
    }

    /// เก็บตำแหน่งหน้าต่าง — eframe เรียกตอนปิด + ทุก 30 วิ (auto_save_interval)
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if let Some(p) = self.saved_pos {
            storage.set_string("overlay_pos", format!("{},{}", p.x, p.y));
        }
    }

    /// วาดจอด้วย painter ตรง ๆ — ทั้งหน้าต่างเป็นแผงเดียว
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if !self.visible {
            return;
        }
        let s = self.scale;
        let ctx = ui.ctx().clone();
        let painter = ui.painter().clone();
        let full = ui.max_rect();

        // ธีม: พื้น/ตัวอักษร/ตัวหนังสือรอง
        let (bg, fg, dim) = if self.light_theme {
            (
                Color32::from_rgba_unmultiplied(250, 250, 252, 228),
                Color32::from_rgb(38, 41, 48),
                Color32::from_rgb(126, 130, 140),
            )
        } else {
            (
                Color32::from_rgba_unmultiplied(22, 24, 30, 208),
                Color32::from_rgb(226, 229, 236),
                Color32::from_rgb(152, 156, 166),
            )
        };

        // กรอบ rounded ทั้งหน้า
        painter.rect_filled(full, 6.0 * s, bg);

        // ---- แถบหัว: ลากย้ายหน้าต่าง + ปุ่มปิดเมื่อ interactive ----
        let header = Rect::from_min_size(full.min, Vec2::new(full.width(), HEADER_H * s));
        let drag = ui.allocate_rect(header, Sense::drag());
        if drag.drag_started() {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        painter.text(
            header.min + Vec2::new(PAD * s, header.height() * 0.5),
            Align2::LEFT_CENTER,
            "sysmon",
            FontId::proportional(10.0 * s),
            dim,
        );
        if self.interactive {
            let x = 18.0 * s;
            let close = Rect::from_min_size(
                full.right_top() - Vec2::new(x + 4.0 * s, 0.0),
                Vec2::new(x, header.height()),
            );
            let btn = ui.allocate_rect(close, Sense::click());
            let c = if btn.hovered() {
                fg
            } else {
                dim
            };
            painter.text(
                close.center(),
                Align2::CENTER_CENTER,
                "✕",
                FontId::proportional(11.0 * s),
                c,
            );
            if btn.clicked() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }

        // ---- แถว metric ----
        let rows = self.metric_rows();
        let mut y = full.top() + header.height() + 4.0 * s;
        let row_h = ROW_H * s;
        let bar_h = 5.0 * s;
        for (name, value, ratio) in rows {
            let row = Rect::from_min_size(
                full.min + Vec2::new(PAD * s, y),
                Vec2::new(full.width() - 2.0 * PAD * s, row_h),
            );
            // ชื่อ metric
            painter.text(
                row.min,
                Align2::LEFT_TOP,
                name,
                FontId::proportional(10.0 * s),
                dim,
            );
            // ค่า
            painter.text(
                row.max + Vec2::new(0.0, -2.0 * s),
                Align2::RIGHT_BOTTOM,
                value,
                FontId::proportional(11.5 * s),
                fg,
            );
            // bar พื้นหลัง + ความยาวตาม ratio
            let bar = Rect::from_min_size(
                row.min + Vec2::new(0.0, row_h - bar_h),
                Vec2::new(row.width(), bar_h),
            );
            painter.rect_filled(bar, bar_h * 0.5, if self.light_theme {
                Color32::from_rgba_unmultiplied(0, 0, 0, 28)
            } else {
                Color32::from_rgba_unmultiplied(255, 255, 255, 30)
            });
            if let Some(r) = ratio {
                let r = r.clamp(0.0, 1.0);
                if r > 0.0 {
                    let fg_rect = Rect::from_min_size(
                        bar.min,
                        Vec2::new(bar.width() * r, bar.height()),
                    );
                    painter.rect_filled(fg_rect, bar_h * 0.5, Self::bar_color(r));
                }
            }
            y += row_h + ROW_GAP * s;
        }

        // ---- หมายเหตุด้านล่าง: collector fail / hotkey ใช้ไม่ได้ / โหมด interactive ----
        let note = if let Some(err) = &self.collector_err {
            format!("collector ผิดพลาด: {err}")
        } else if self.manager.is_none() {
            "hotkey ไม่พร้อมใช้งาน — ปิดด้วย Alt+F4".into()
        } else if self.interactive {
            format!(
                "เปิด interactive — ลากย้าย/ปิดได้ (กด {} กลับ)",
                hotkey_short(&self.keys.interact)
            )
        } else {
            String::new()
        };
        if !note.is_empty() {
            let y = full.bottom() - 14.0 * s;
            painter.text(
                full.min + Vec2::new(PAD * s, y),
                Align2::LEFT_BOTTOM,
                &note,
                FontId::proportional(8.5 * s),
                dim,
            );
        }
    }
}

/// แปลง HotKey กลับเป็นข้อความแสดงใน note (ใช้ Display ของ HotKey)
fn hotkey_short(hk: &HotKey) -> String {
    format!("{hk}")
}

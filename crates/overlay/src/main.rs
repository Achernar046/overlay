//! sysmon-overlay — หน้าต่าง overlay ระหว่างใช้งานเกม/โปรแกรมอื่น (Phase 5)
//!
//! - หน้าต่างโปร่งแสง ไม่มีกรอบ always-on-top ไม่ขึ้น taskbar
//! - เริ่มต้นเป็น click-through กันบังเกม — กด interact hotkey เพื่อลาก/ปิด
//! - hotkey กลางระบบ (`global-hotkey`)**: สลับแสดง/ซ่อน + สลับโหมด interactive
//! - ไม่มี DLL inject ตามข้อกังวล anti-cheat ใน plan — เป็น top-level window อย่างเดียว

mod app;

use eframe::egui;
use global_hotkey::hotkey::HotKey;
use global_hotkey::GlobalHotKeyManager;
use sysmon_core::Config;

use crate::app::{Keys, OverlayApp};

fn main() -> eframe::Result<()> {
    let mut warn = |msg: &str| eprintln!("[config] {msg}");
    let config = Config::load_or_default("sysmon.toml", &mut warn);
    let o = &config.overlay;
    let scale = o.scale.clamp(0.5, 2.0);

    let toggle = hotkey_or_default(&o.hotkey, "ctrl+alt+o", &mut warn);
    let interact = hotkey_or_default(&o.interact_hotkey, "ctrl+alt+p", &mut warn);

    let manager = match GlobalHotKeyManager::new() {
        Ok(m) => Some(m),
        Err(e) => {
            eprintln!("[hotkey] เปิดระบบ global hotkey ไม่สำเร็จ: {e} — ปุ่มลัดจะไม่ทำงาน (ปิดโปรแกรมด้วย Alt+F4)");
            None
        }
    };

    if let Some(m) = &manager {
        if let Err(e) = m.register(toggle) {
            eprintln!("[hotkey] register ปุ่มสลับแสดงผลไม่สำเร็จ: {e}");
        }
        if let Err(e) = m.register(interact) {
            eprintln!("[hotkey] register ปุ่ม interactive ไม่สำเร็จ: {e}");
        }
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([WIN_W * scale, WIN_H * scale])
            .with_resizable(false)
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_taskbar(false)
            .with_position([o.margin, o.margin]),
        ..Default::default()
    };

    eframe::run_native(
        "sysmon overlay",
        options,
        Box::new(move |cc| {
            Ok(Box::new(OverlayApp::new(
                cc,
                config.collector.clone(),
                config.sample_interval_ms.max(200),
                config.overlay.theme.eq_ignore_ascii_case("light"),
                scale,
                Keys { toggle, interact },
                manager,
            )))
        }),
    )
}

/// ขนาดกรอบพื้นฐาน (point) คูณ scale — ต้องตรงกับค่า const ใน app.rs
const WIN_W: f32 = 320.0;
const WIN_H: f32 = 190.0;

/// แปลง config เป็น `HotKey` — parser ในตัวของ global-hotkey รู้จัก "ctrl+alt+o" อยู่แล้ว
fn hotkey_or_default(text: &str, default: &'static str, warn: &mut dyn FnMut(&str)) -> HotKey {
    // เผื่อ loader ของ global-hotkey แปลง "win" ไม่ได้ → ใช้ super/meta แทน
    let normalized = text.trim().to_ascii_lowercase();
    let normalized = normalized.replace("win+", "super+");
    HotKey::try_from(&normalized[..]).unwrap_or_else(|_| {
        warn(&format!("hotkey \"{text}\" แปลงไม่ได้ → ใช้ {default}"));
        HotKey::try_from(default).unwrap()
    })
}

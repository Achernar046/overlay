//! System tray (Windows) — icon + เมนู: เปิด ui/overlay, toggle autostart, ออก
//!
//! tray-icon หนา Windows ต้องมี event loop ทำงานบน thread ของผู้สร้าง icon
//! ↔ thread นี้เลยต้อง pump เอง (PeekMessageW) และดูแล menu events เท่าทันเวลา;
//! monitor loop วิ่งอยู่บน worker thread ต่างหาก แล้วส่งสรุป CPU/RAM มาอัพเดต tooltip

use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder};

use sysmon_core::ipc::SharedLatest;
use sysmon_core::Config;

use crate::autostart;
use crate::monitor::MonitorLoop;

/// สี icon 16 เมทริกซ์ — ต้องตรงกับขนาดกราฟ
const ICON_W: u32 = 32;
const ICON_H: u32 = 32;

/// รีเฟรช tooltip แบบเบา — PeekMessage ทุกเลูป, sleep ระหว่างรอบ
const PUMP_SLEEP: Duration = Duration::from_millis(50);

pub fn run(config: Config, quit: Arc<AtomicBool>, measure: bool, latest: Option<SharedLatest>) -> Result<()> {
    unsafe { hide_console() };

    // monitor โหลดบน worker thread
    let (status_tx, status_rx) = channel::<String>();
    let worker: Result<JoinHandle<()>, _> = {
        let cfg = config.clone();
        let q = Arc::clone(&quit);
        thread::Builder::new()
            .name("sysmon-monitor".into())
            .spawn(move || {
                if let Err(e) =
                    MonitorLoop::new(cfg, None, true, measure, Some(status_tx), latest).run(q)
                {
                    eprintln!("[daemon] monitor หยุดผิดปกติ: {e}");
                }
            })
    };
    let worker = worker?;

    // เมนู
    let mi_ui = MenuItem::new("เปิดหน้าต่าง Live (ui)", true, None);
    let mi_overlay = MenuItem::new("เปิด overlay", true, None);
    let chk_auto = CheckMenuItem::new(
        "เปิดเองเมื่อล็อกอิน (autostart)",
        true,
        autostart::is_on(),
        None,
    );
    let mi_quit = MenuItem::new("ออก", true, None);

    let menu = Menu::new();
    menu.append(&mi_ui).map_err(menu_err)?;
    menu.append(&mi_overlay).map_err(menu_err)?;
    menu.append(&PredefinedMenuItem::separator()).map_err(menu_err)?;
    menu.append(&chk_auto).map_err(menu_err)?;
    menu.append(&PredefinedMenuItem::separator()).map_err(menu_err)?;
    menu.append(&mi_quit).map_err(menu_err)?;

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_icon(app_icon().context("สร้าง icon ไม่สำเร็จ")?)
        .with_tooltip("sysmon — system monitor")
        .build()
        .context("สร้าง tray icon ไม่สำเร็จ")?;

    eprintln!("[tray] เปิด system tray แล้ว (right-click เพื่อเมนู)");

    loop {
        // สถานะ quit ตรวจทุก pump
        if quit.load(Ordering::Relaxed) {
            break;
        }

        // เมนู events
        while let Ok(ev) = MenuEvent::receiver().try_recv() {
            let id = ev.id().clone();
            if id == mi_ui.id().clone() {
                spawn_sibling("sysmon-ui.exe");
            } else if id == mi_overlay.id().clone() {
                spawn_sibling("sysmon-overlay.exe");
            } else if id == chk_auto.id().clone() {
                let want = !autostart::is_on();
                match autostart::set(want) {
                    Ok(_) => {
                        chk_auto.set_checked(want);
                        eprintln!("[tray] autostart = {want}");
                    }
                    Err(e) => {
                        eprintln!("[tray] ตั้ง autostart ไม่สำเร็จ: {e:#}");
                        chk_auto.set_checked(!want);
                    }
                }
            } else if id == mi_quit.id().clone() {
                quit.store(true, Ordering::Relaxed);
            }
        }

        // tooltip
        if let Ok(status) = status_rx.try_recv() {
            let _ = tray.set_tooltip(Some(status.as_str()));
        }

        unsafe { pump_messages() };
        thread::sleep(PUMP_SLEEP);
    }

    // ปิด: ยกเลิก icon ก่อน join worker (worker จะ flush ข้อมูลค้างก่อนจบ)
    drop(tray);
    let _ = worker.join();
    Ok(())
}

/// ยิง exe ข้าง ๆ daemon ขึ้นมา (ui / overlay)
fn spawn_sibling(exe: &str) {
    let dir = match std::env::current_exe()
        .map_err(|e| anyhow!("หา exe daemon ไม่ได้: {e}"))
        .and_then(|exe| exe.parent().map(|p| p.to_path_buf()).ok_or_else(|| anyhow!("exe ไม่มี parent ให้เอา")))
    {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[tray] {e}");
            return;
        }
    };
    let path = dir.join(exe);
    if !path.exists() {
        eprintln!("[tray] ไม่พบ {} — แน่นอนว่าแพ็กแยกไฟล์ไว้การาย่ากัน", path.display());
        return;
    }
    match Command::new(&path).spawn() {
        Ok(_) => eprintln!("[tray] เปิด {exe} แล้ว"),
        Err(e) => eprintln!("[tray] เปิด {exe} ไม่สำเร็จ: {e}"),
    }
}

/// Pump เข้าข้อความ Windows ให้ tray icon ยังรับ event ได้ — Peek เท่าที่มี
unsafe fn pump_messages() {
    unsafe {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            DispatchMessageW, MSG, PeekMessageW, PM_REMOVE, TranslateMessage,
        };
        let mut msg: MSG = std::mem::zeroed();
        while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// ซ่อน console window เมื่อรันในโหมด tray (autostart จะไม่โผล่หน้าดำขึ้นมา)
unsafe fn hide_console() {
    unsafe {
        use windows_sys::Win32::System::Console::GetConsoleWindow;
        use windows_sys::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};
        let hwnd = GetConsoleWindow();
        if !hwnd.is_null() {
            ShowWindow(hwnd, SW_HIDE);
        }
    }
}

/// icon ที่วาดจาก buffer สี่เหลี่ยมขนาดเล็ก — ไม่ต้องพกพาไฟล์ .ico
fn app_icon() -> Result<Icon> {
    let w = ICON_W as usize;
    let h = ICON_H as usize;
    let mut rgba = vec![0u8; w * h * 4];

    let cx = (ICON_W - 1) as f32 / 2.0;
    let cy = (ICON_H - 1) as f32 / 2.0;
    let radius = 14.0;
    // ด้านบนของแท่งกราฟใน top-information grid 4 แท่ง
    let bars: [(f32, f32, f32, [u8; 3]); 4] = [
        // (x, width, height, color)
        (7.0, 4.0, 7.0, [84, 196, 120]),
        (12.5, 4.0, 11.0, [84, 196, 120]),
        (18.0, 4.0, 15.0, [232, 181, 67]),
        (23.5, 4.0, 19.0, [229, 72, 80]),
    ];

    for (i, px) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let x = (i % w) as f32;
        let y = (i / w) as f32;
        let dx = x - cx;
        let dy = y - cy;
        let inside = (dx * dx + dy * dy).sqrt() <= radius;
        if !inside {
            continue;
        }
        let (r, g, b, a) = if y >= 24.0 {
            // พื้นลงด้าน — ให้เทาอ่อนขึ้น
            (52.0, 56.0, 64.0, 255.0)
        } else {
            match bars
                .iter()
                .find(|(bx, bw, bh, _)| x >= *bx && x < *bx + *bw && y >= 24.0 - *bh)
            {
                Some((_, _, _, c)) => (c[0] as f32, c[1] as f32, c[2] as f32, 255.0),
                None => (24.0, 26.0, 32.0, 255.0),
            }
        };
        px[0] = r as u8;
        px[1] = g as u8;
        px[2] = b as u8;
        px[3] = a as u8;
    }

    Icon::from_rgba(rgba, ICON_W, ICON_H).context("สร้าง icon ล้มเหลว")
}

fn menu_err(e: tray_icon::menu::Error) -> anyhow::Error {
    anyhow!("จัดการ menu ผิดปกติ: {e}")
}

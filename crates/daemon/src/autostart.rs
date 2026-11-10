//! Autostart — คุมแถว `Run` ของ HKCU (Windows) ให้ daemon เปิดเองตอนล็อกอิน
//!
//! ใช้ key ของผู้ใช้ (ไม่ใช่ global ทั้งเครื่อง) เพื่อไม่ต้องขอสิทธิ์ admin

use anyhow::{anyhow, Context, Result};
use winreg::enums::{HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE};
use winreg::RegKey;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "sysmon";

/// คำสั่งเดียวจบ: `--autostart on|off|show`
pub fn handle(mode: &str) -> Result<()> {
    match mode {
        "on" => {
            let path = set(true)?
                .ok_or_else(|| anyhow!("set(true) ต้องคืน path"))?;
            eprintln!("[autostart] เปิดแล้ว → จะเริ่มที่ล็อกอิน: {path}");
            Ok(())
        }
        "off" => match set(false) {
            Ok(Some(_)) => {
                eprintln!("[autostart] ปิดแล้ว");
                Ok(())
            }
            _ => {
                eprintln!("[autostart] ตอนนี้ไม่ได้เปิด autostart อยู่แล้ว");
                Ok(())
            }
        },
        "show" | "print" => {
            match is_on().then_some(current_value()).flatten() {
                Some(p) => eprintln!("[autostart] เปิดอยู่: {p}"),
                None => eprintln!("[autostart] ปิดอยู่"),
            }
            Ok(())
        }
        other => Err(anyhow!(
            "--autostart ใช้กับ on | off | show เท่านั้น (ได้รับ \"{other}\")"
        )),
    }
}

/// อ่านสถานะปัจจุบัน (มี value อยู่?)
pub fn is_on() -> bool {
    value().is_some()
}

/// เปิด/ปิด — ตอนเปิดจะคืน path ที่ลงทะเบียน
pub fn set(on: bool) -> Result<Option<String>> {
    if on {
        let exe = std::env::current_exe()?;
        let path = exe.to_string_lossy().to_string();
        open_key(true)?.set_value(VALUE_NAME, &path)?;
        Ok(Some(path))
    } else {
        match value() {
            Some(v) => {
                open_key(true)?.delete_value(VALUE_NAME)?;
                Ok(Some(v))
            }
            None => Ok(None),
        }
    }
}

/// ค่าที่ลงทะเบียนอยุ่ ถ้ามี
fn value() -> Option<String> {
    let key = open_key(false).ok()?;
    key.get_value::<String, _>(VALUE_NAME).ok()
}

fn current_value() -> Option<String> {
    value()
}

fn open_key(write: bool) -> Result<RegKey> {
    let access = if write {
        KEY_QUERY_VALUE | KEY_SET_VALUE
    } else {
        KEY_QUERY_VALUE
    };
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(RUN_KEY, access)
        .with_context(|| format!("เปิด HKCU\\{RUN_KEY} ไม่สำเร็จ"))
}

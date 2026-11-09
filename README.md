# sysmon

Real-time system monitor ด้วย Rust — ตามแผนใน `../rust-system-monitor-plan.md`

**สถานะ:** Phase 1 (workspace + collector + TUI ทดสอบ)

## โครงสร้าง

```
crates/
├─ core/       # types (Snapshot), config, IPC protocol (placeholder)
├─ collector/  # CPU/RAM/Disk/GPU/Network ผ่าน sysinfo
├─ daemon/     # โปรแกรมหลักเก็บข้อมูล (Phase 1: พิมพ์ headline ออก stdout)
└─ tui/        # terminal dashboard ตัวทดสอบ (Phase 4 จะเปลี่ยนเป็น egui)
```

## รัน

จากโฟลเดอร์ `sysmon/`:

```sh
cargo run -p sysmon-tui      # dashboard ใน terminal, กด q/Esc ออก
cargo run -p sysmon-daemon   # daemon Phase 1 พิมพ์สถานะทาง stdout
cargo test --all             # unit tests (config, formatter)
```

## config

อ่าน `sysmon.toml` จาก working directory ถ้ามี (ไม่มี = ใช้ค่า default):

```toml
sample_interval_ms = 1000

[collector]
monitor_disks = true
monitor_networks = true
monitor_temps = true   # บางเครื่องต้องรันด้วยสิทธิ์ admin
```

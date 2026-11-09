# sysmon

Real-time system monitor ด้วย Rust — ตามแผนใน `../rust-system-monitor-plan.md`

**สถานะ:** Phase 2 (Phase 1 เสร็จ: workspace + collector + TUI; Phase 2: process monitor + NVML)

## โครงสร้าง

```
crates/
├─ core/       # types (Snapshot, ProcessStat), config, IPC protocol (placeholder)
├─ collector/  # CPU/RAM/Disk/GPU/Network ผ่าน sysinfo; NVIDIA เพิ่มด้วย NVML; Top-N process
├─ daemon/     # โปรแกรมหลักเก็บข้อมูล (--top N = พิมพ์ top process ทุก tick)
└─ tui/        # terminal dashboard (Phase 4 จะเปลี่ยนเป็น egui)
```

## รัน

จากโฟลเดอร์ `sysmon/`:

```sh
cargo run -p sysmon-tui         # dashboard ใน terminal
cargo run -p sysmon-daemon      # daemon พิมพ์สถานะทาง stdout
cargo run -p sysmon-daemon -- --top 10   # + top process ราย tick
cargo test --all                # unit tests (config, formatter)
```

**ปุ่มใน TUI:** `q`/`Esc` ออก · `Tab` สลับหน้า System ↔ Processes · `m` สลับ sort CPU/MEM (ในหน้า Processes)

**หมายเหตุ:** ค่า process (CPU/แรม) แม่นตั้งแต่ sample ครั้งที่สอง (sysinfo ต้องวัด delta); GPU NVIDIA ต้องมี driver — ไม่มี NVML ก็ยังได้ usage/VRAM จาก D3DKMT

## config

อ่าน `sysmon.toml` จาก working directory ถ้ามี (ไม่มี = ใช้ค่า default):

```toml
sample_interval_ms = 1000

[collector]
monitor_disks = true
monitor_networks = true
monitor_temps = true   # บางเครื่องต้องรันด้วยสิทธิ์ admin
```

# sysmon

Real-time system monitor ด้วย Rust — ตามแผนใน `../rust-system-monitor-plan.md`

**สถานะ:** Phase 4 (collector/process/NVML + SQLite storage + egui dashboard เสร็จ)

## โครงสร้าง

```
crates/
├─ core/       # types (Snapshot, ProcessStat), config, IPC protocol (placeholder)
├─ collector/  # CPU/RAM/Disk/GPU/Network ผ่าน sysinfo; NVIDIA เพิ่มด้วย NVML; Top-N process
├─ storage/    # SQLite (WAL) — เขียน batch, rollup 1m/1h, retention, read APIs
├─ daemon/     # เก็บข้อมูล + เขียน DB รอบ flush (default 15 วิ) (--top N = top process)
├─ tui/        # terminal dashboard
└─ ui/         # egui dashboard — Live plots + ประวัติจาก SQLite
```

## รัน

จากโฟลเดอร์ `sysmon/`:

```sh
cargo run -p sysmon-tui          # dashboard ใน terminal
cargo run -p sysmon-ui           # egui dashboard (หน้าต่าง GUI)
cargo run -p sysmon-daemon       # daemon พิมพ์สถานะทาง stdout
cargo run -p sysmon-daemon -- --top 10   # + top process ราย tick
cargo test --all                 # unit tests (config, formatter, storage, ui)
```

**ปุ่มใน TUI:** `q`/`Esc` ออก · `Tab` สลับหน้า System ↔ Processes · `m` สลับ sort CPU/MEM (ในหน้า Processes)

**ปุ่มใน egui UI:** แถบบนสลับหน้า Live ↔ ประวัติ · หน้า Live = กราฟ CPU/Temp, RAM, Network + bar ต่อ core/GPU/ดิสก์ · หน้า ประวัติ = เลือกช่วง 1 ชม. / 24 ชม. / 30 วัน / 1 ปี (1 ชม. = raw, ช่วงยาว = rollup) กราฟ CPU กราฟ RAM/Network; กราฟซูม/แพนได้ทั้งหมด (double-click reset)

**หมายเหตุ:** ค่า process (CPU/แรม) แม่นตั้งแต่ sample ครั้งที่สอง (sysinfo ต้องวัด delta); GPU NVIDIA ต้องมี driver — ไม่มี NVML ก็ยังได้ usage/VRAM จาก D3DKMT

**ประวัติ (Phase 3):** daemon เขียนลง `sysmon.db` (SQLite, WAL) ทุก 15 วิ — แก้ได้ใน `[storage]` ของ `sysmon.toml`; ตรวจไฟล์ด้วย `python scripts/check_db.py [db_path]`; UI อ่านจากไฟล์เดียวกันโดยตรง

## config

อ่าน `sysmon.toml` จาก working directory ถ้ามี (ไม่มี = ใช้ค่า default):

```toml
sample_interval_ms = 1000

[collector]
monitor_disks = true
monitor_networks = true
monitor_temps = true   # บางเครื่องต้องรันด้วยสิทธิ์ admin
```

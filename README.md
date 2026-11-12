# sysmon

Real-time system monitor ด้วย Rust — ตามแผนใน `../rust-system-monitor-plan.md`

**สถานะ:** 6 Phases ครบทั้งแผน + IPC named pipe (daemon → ui/overlay อ่านค่าสดจาก daemon ตัวเดียว)

## โครงสร้าง

```
crates/
├─ core/       # types (Snapshot, ProcessStat), config, IPC protocol (placeholder)
├─ collector/  # CPU/RAM/Disk/GPU/Network ผ่าน sysinfo; NVIDIA เพิ่มด้วย NVML; Top-N process
├─ storage/    # SQLite (WAL) — เขียน batch, rollup 1m/1h, retention, read APIs
├─ daemon/     # เก็บข้อมูล + เขียน DB รอบ flush (default 15 วิ) (--top N = top process)
├─ tui/        # terminal dashboard
├─ ui/         # egui dashboard — Live plots + ประวัติจาก SQLite
└─ overlay/    # หน้าต่างโปร่งแสง always-on-top + hotkey กลางระบบ (global-hotkey)
```

## รัน

จากโฟลเดอร์ `sysmon/`:

```sh
cargo run -p sysmon-tui          # dashboard ใน terminal
cargo run -p sysmon-ui           # egui dashboard (หน้าต่าง GUI)
cargo run -p sysmon-overlay      # overlay แบบ click-through (Ctrl+Alt+O แสดง/ซ่อน, Ctrl+Alt+P โหมด interactive)

# daemon
cargo run -p sysmon-daemon       # โหมด console: พิมพ์สถานะทาง stdout
cargo run -p sysmon-daemon -- --top 10    # + top process ราย tick
cargo run -p sysmon-daemon -- --tray      # system tray (Windows): icon + เมนู, ซ่อน console
cargo run -p sysmon-daemon -- --autostart on   # เปิดเองตอนล็อกอิน (HKCU Run key, ไม่ต้อง admin)
cargo run -p sysmon-daemon -- --autostart show # ดูสถานะ (`off` = ปิด)
cargo run -p sysmon-daemon -- --measure   # profiling: เวลา snapshot / ขนาด flush ทุก 60 ticks
cargo run -p sysmon-daemon -- --print-config   # พิมพ์ sysmon.toml ตัวอย่าง
cargo test --all                 # unit tests (config, formatter, storage, ui, daemon flush-exit)
```

**สำคัญ:** ออกจาก daemon ด้วย `Ctrl+C` หรือเมนู "ออก" ของ tray เสมอ — จะเขียนข้อมูลที่ค้างใน buffer ลง DB ก่อนปิด (kill แบบ force ยังหายข้อมูล ~flush_secs วินาทีสุดท้ายอยู่)

**ปุ่มใน TUI:** `q`/`Esc` ออก · `Tab` สลับหน้า System ↔ Processes · `m` สลับ sort CPU/MEM (ในหน้า Processes)

**ปุ่มใน egui UI:** แถบบนสลับหน้า Live ↔ ประวัติ · หน้า Live = กราฟ CPU/Temp, RAM, Network + bar ต่อ core/GPU/ดิสก์ · หน้า ประวัติ = เลือกช่วง 1 ชม. / 24 ชม. / 30 วัน / 1 ปี (1 ชม. = raw, ช่วงยาว = rollup) กราฟ CPU กราฟ RAM/Network; กราฟซูม/แพนได้ทั้งหมด (double-click reset)

**หมายเหตุ:** ค่า process (CPU/แรม) แม่นตั้งแต่ sample ครั้งที่สอง (sysinfo ต้องวัด delta); GPU NVIDIA ต้องมี driver — ไม่มี NVML ก็ยังได้ usage/VRAM จาก D3DKMT

**ประวัติ (Phase 3):** daemon เขียนลง `sysmon.db` (SQLite, WAL) ทุก 15 วิ — แก้ได้ใน `[storage]` ของ `sysmon.toml`; ตรวจไฟล์ด้วย `python scripts/check_db.py [db_path]`; UI อ่านจากไฟล์เดียวกันโดยตรง

**Overlay (Phase 5):** หน้าต่างโปร่งแสงไม่มีกรอบ always-on-top แสดง CPU/RAM/GPU/NET พร้อม bar — เริ่มต้นเป็น *click-through* (คลิกทะลุไปเกมได้) อัพเดตทุก sample interval แม้ซ่อนอยู่

- `Ctrl+Alt+O` สลับแสดง/ซ่อน · `Ctrl+Alt+P` สลับโหมด interactive (ลากหน้าต่างจากแถบหัว "sysmon", ปิดด้วยปุ่ม ✕)
- แก้ปุ่มลัด/ธีม/ขนาด/ตำแหน่งได้ใน `[overlay]` ของ `sysmon.toml` ด้านล่าง
- เป็น top-level window อย่างเดียว ไม่ inject DLL → ไม่ชน anti-cheat; ใช้ไม่ได้กับเกมที่เป็น *exclusive fullscreen* — สลับเป็น borderless/windowed

**System tray + autostart (Phase 6):** รัน `sysmon-daemon --tray` (Windows) — icon ใน tray พร้อม tooltip CPU/RAM อัพเดตทุก 2 วิ, เมนู: เปิดหน้าต่าง Live / เปิด overlay / toggle *"เปิดเองเมื่อล็อกอิน"* / ออก (flush ก่อนปิด) · console จะถูกซ่อนตอนเริ่ม

**แจกจ่าย (portable):** `powershell -ExecutionPolicy Bypass -File scripts\package.ps1` → build release แล้วแพ็ก `dist/sysmon-<เวอร์ชัน>-windows-x64.zip` (daemon/ui/overlay/tui + README + `sysmon.toml.sample`) — ไว้อัพขึ้น GitHub Releases ตามแผน

**IPC ค่าสด:** แนวปฏิบัติตามสถาปัตยกรรมใน plan — daemon เปิด named pipe (`[ipc] name`, default `sysmon-v1`) แล้วส่ง `Snapshot` ทุก tick (postcard + length-prefix) ให้ ui/overlay ที่ต่ออยู่ · ต่อไม่ได้ (daemon ไม่รัน) → แอป fallback ไป collector ในตัวเองและลองต่อใหม่ทุก 10 วิ · ตั้ง `enabled = false` ปิดได้

## config

อ่าน `sysmon.toml` จาก working directory ถ้ามี (ไม่มี = ใช้ค่า default):

```toml
sample_interval_ms = 1000

[collector]
monitor_disks = true
monitor_networks = true
monitor_temps = true   # บางเครื่องต้องรันด้วยสิทธิ์ admin

[overlay]              # Phase 5
margin = 24.0          # ระยะห่างจากมุมจอ (point)
scale = 1.0            # ขนาดเนื้อหา 0.5–2.0
theme = "dark"         # "dark" หรือ "light"
hotkey = "ctrl+alt+o"           # ctrl/alt/shift/super + ตัวอักษร/เลข/f1–f12/arrows
interact_hotkey = "ctrl+alt+p"  # สลับ click-through ↔ interactive

[ipc]                  # daemon → ui/overlay ผ่าน named pipe
enabled = true         # daemon เปิด pipe; ui/overlay ต่อไม่ได้จะ fallback sample เอง
name = "sysmon-v1"     # ชื่อ socket — ต้องตรงกันทุกโปรแกรม
```

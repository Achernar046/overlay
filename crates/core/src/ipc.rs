//! IPC protocol placeholder — ต่อยอดใน Phase 3 (shared memory + named pipe)
//!
//! วางแผนไว้ตาม plan:
//! - header มี `PROTOCOL_VERSION` + sequence counter (กัน UI อ่านค่าครึ่ง ๆ กลาง ๆ)
//! - daemon เขียน `Snapshot` (serde) ลง shared memory, UI/overlay อ่านฝั่งเดียว

pub const PROTOCOL_VERSION: u32 = 1;

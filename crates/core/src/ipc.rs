//! IPC protocol (named pipe) — daemon ส่ง `Snapshot` ให้ ui/overlay อ่านสด
//!
//! รูปแบบการติดต่อ:
//! - daemon เปิด listener ชื่อ `[ipc] name` ใน config (default `"sysmon-v1"`, namespaced)
//! - client เชื่อมต่อแล้ว daemon ส่ง frame แรกเป็น `Hello { version, interval_ms }`
//!   จากนั้นส่ง `Snapshot` ทุก tick ที่ข้อมูลใหม่ (length-prefixed, postcard)
//! - client อ่านฝั่งเดียว เก็บไว้ใน `SnapshotFeed` — ตาย (daemon ปิด) → แอปต้อง
//!   fallback ไป collector ในตัวเองได้

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{
    GenericNamespaced, ListenerNonblockingMode, ListenerOptions,
};
use serde::{Deserialize, Serialize};

use crate::types::Snapshot;

/// เวอร์ชันของ protocol — เปลี่ยนเมื่อรูปแบบ frame ไม่ compatible
pub const PROTOCOL_VERSION: u32 = 1;
/// ชื่อ socket ตั้งต้น (ต้องตรงกับ `[ipc] name` ใน config)
pub const DEFAULT_NAME: &str = "sysmon-v1";
/// ฝา frame ใหญ่สุด — กัน stream เสียแล้วอ่านเป็นขยะล้างเดือย
const MAX_FRAME: usize = 1024 * 1024;

/// แพ็กเกจหนึ่งชิ้นบน stream — แรกเป็น Hello, ตามด้วย Snapshot ต่อเนื่อง
// Snapshot ใหญ่กว่า Hello มาก — ยังคงฝาเป็น struct ตรง ๆ ไปก่อน (ค่าขนาดไม่ใช่ปัญหาจริง)
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Frame {
    Hello { version: u32, interval_ms: u32 },
    Snapshot(Snapshot),
}

/// ข้อผิดพลาดฝั่ง encode/decode frame
#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    #[error("โครงสร้าง stream เสียหาย (frame ผิดรูป)")]
    Corrupt,
}

impl Frame {
    /// คืนช่วงของ `Snapshot` ตาม daemon แจ้งใน Hello (ยังไม่เปิด? → None)
    pub fn snapshot(self) -> Option<Snapshot> {
        match self {
            Frame::Snapshot(s) => Some(s),
            _ => None,
        }
    }
}

/// ระยะโหลด: postcard + u32 LE length prefix
pub fn encode_frame(frame: &Frame) -> Result<Vec<u8>, IpcError> {
    let payload = postcard::to_allocvec(frame).map_err(|_| IpcError::Corrupt)?;
    let len = payload.len() as u32;
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

/// ระยะโหลดฝั่งเสว่ง — อ่านเมื่อมีข้อมูลครบ 1 frame แล้วเท่านั้น
pub struct FrameReader {
    buf: Vec<u8>,
}

impl FrameReader {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// เทข้อมูลที่รับมา → คืน frame ที่ครบแล้ว
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Frame>, IpcError> {
        self.buf.extend_from_slice(bytes);
        let mut frames = Vec::new();
        loop {
            if self.buf.len() < 4 {
                return Ok(frames);
            }
            let len = u32::from_le_bytes(self.buf[..4].try_into().unwrap()) as usize;
            if len > MAX_FRAME {
                return Err(IpcError::Corrupt);
            }
            if self.buf.len() < 4 + len {
                return Ok(frames);
            }
            let frame: Frame = postcard::from_bytes(&self.buf[4..4 + len])
                .map_err(|_| IpcError::Corrupt)?;
            self.buf.drain(..4 + len);
            frames.push(frame);
        }
    }
}

impl Default for FrameReader {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// ฝั่ง daemon — listener รับ clients เข้ามาแล้ว push snapshot ล่าสุด
// ---------------------------------------------------------------------------

/// ช่องเก็บ snapshot ล่าสุดที่ MonitorLoop อัพเดตและ clients อ่าน
pub type SharedLatest = Arc<Mutex<Option<Snapshot>>>;
/// ตัวนับ client ที่ต่อ pipe อยู่ — ใช้ตัดสินใจว่าจะลดความถี่ sampling ได้ไหม
pub type SharedClients = Arc<AtomicUsize>;

pub struct IpcServer {
    latest: SharedLatest,
    clients: SharedClients,
    stop: Arc<AtomicBool>,
    accept_thread: Option<std::thread::JoinHandle<()>>,
}

impl IpcServer {
    /// เปิด listener แล้ว spawn thread รับ clients — ใครถือ `latest_slot()` ให้
    /// มีหน้าที่ใส่ snapshot ล่าสุดทุก tick
    pub fn spawn(name: &str, interval_ms: u32) -> Result<Self, std::io::Error> {
        let ns_name = name
            .to_ns_name::<GenericNamespaced>()
            .map_err(|e| std::io::Error::other(format!("แปลงชื่อ socket ไม่ได้: {e}")))?;
        let listener = ListenerOptions::new()
            .name(ns_name)
            .nonblocking(ListenerNonblockingMode::Accept)
            .create_sync()?;

        let latest: SharedLatest = Arc::new(Mutex::new(None));
        let clients: SharedClients = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));

        let accept_thread = {
            let latest = Arc::clone(&latest);
            let clients = Arc::clone(&clients);
            let stop = Arc::clone(&stop);
            thread::Builder::new()
                .name("sysmon-ipc".into())
                .spawn(move || {
                    let poll = Duration::from_millis(100);
                    while !stop.load(Ordering::Relaxed) {
                        match listener.accept() {
                            Ok(stream) => {
                                let latest = Arc::clone(&latest);
                                let clients = Arc::clone(&clients);
                                let _ = thread::Builder::new()
                                    .name("sysmon-ipc-client".into())
                                    .spawn(move || serve_client(stream, latest, clients, interval_ms));
                            }
                            Err(_) => thread::sleep(poll),
                        }
                    }
                })
        }?;

        Ok(Self {
            latest,
            clients,
            stop,
            accept_thread: Some(accept_thread),
        })
    }

    /// ช่องให้ monitor loop อัพเดต snapshot ล่าสุด
    pub fn latest_slot(&self) -> SharedLatest {
        Arc::clone(&self.latest)
    }

    /// ช่องตัวนับ client — MonitorLoop ใช้ลดความถี่ตอนไม่มีคนต่อ
    pub fn clients_slot(&self) -> SharedClients {
        Arc::clone(&self.clients)
    }

    /// มี client ต่อ pipe อยู่หรือไม่
    pub fn has_clients(&self) -> bool {
        self.clients.load(Ordering::Relaxed) > 0
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.accept_thread.take() {
            let _ = t.join();
        }
    }
}

/// ส่ง Hello แล้ว push snapshot ใหม่ ๆ ให้ client ตัวเดียว จนขาด
fn serve_client(
    mut stream: LocalSocketStream,
    latest: SharedLatest,
    clients: SharedClients,
    interval_ms: u32,
) {
    // นับเข้า-นับออกให้ตรง แม้ thread จบกลาง error
    clients.fetch_add(1, Ordering::Relaxed);
    let _guard = ClientGuard(&clients);
    let hello = Frame::Hello {
        version: PROTOCOL_VERSION,
        interval_ms,
    };
    if write_frame(&mut stream, &hello).is_err() {
        return;
    }
    let mut last_ts = 0i64;
    loop {
        thread::sleep(Duration::from_millis(100));
        let snap = latest.lock().unwrap().clone();
        let Some(s) = snap else { continue };
        if s.ts == last_ts {
            continue;
        }
        if write_frame(&mut stream, &Frame::Snapshot(s.clone())).is_err() {
            return; // client ปิด/ขาด — จบ thread นี้
        }
        last_ts = s.ts;
    }
}

/// นับเข้า-นับออกของ client connection — Drop กัน thread จบแบบ early return
struct ClientGuard<'a>(&'a SharedClients);

impl Drop for ClientGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

fn write_frame(stream: &mut LocalSocketStream, frame: &Frame) -> Result<(), std::io::Error> {
    let bytes =
        encode_frame(frame).map_err(|_| std::io::Error::other("frame แปลงไม่ได้"))?;
    stream.write_all(&bytes)
}

// ---------------------------------------------------------------------------
// ฝั่ง client — ui/overlay
// ---------------------------------------------------------------------------

/// การเชื่อมต่อจาก daemon — thread อ่านทำงานตลอดจน stream ขาด
/// (blocking read บน thread แยก กัน stall UI)
pub struct SnapshotFeed {
    state: Arc<Mutex<FeedState>>,
}

struct FeedState {
    latest: Option<Snapshot>,
    interval_ms: u32,
    alive: bool,
}

impl SnapshotFeed {
    /// เชื่อมต่อ listener ของ daemon — ผิดพลาด (เช่น daemon ไม่รัน) → ให้เรียกใหม่ภายหลัง
    pub fn connect(name: &str) -> Result<Self, std::io::Error> {
        let ns_name = name
            .to_ns_name::<GenericNamespaced>()
            .map_err(|e| std::io::Error::other(format!("แปลงชื่อ socket ไม่ได้: {e}")))?;
        let stream = LocalSocketStream::connect(ns_name)?;

        let state = Arc::new(Mutex::new(FeedState {
            latest: None,
            interval_ms: 0,
            alive: true,
        }));
        // reader thread detach — จบเองเมื่อ stream ขาด/EOF; drop แอประหว่าง read ได้ปกติ
        let _ = thread::Builder::new()
            .name("sysmon-ipc-feed".into())
            .spawn({
                let state = Arc::clone(&state);
                move || read_loop(stream, state)
            });
        Ok(Self { state })
    }

    pub fn latest(&self) -> Option<Snapshot> {
        self.state.lock().unwrap().latest.clone()
    }

    /// ช่วง sample ที่ daemon ใช้อยู่ (รู้หลังได้รับ Hello แล้วเท่านั้น)
    pub fn interval_ms(&self) -> u32 {
        self.state.lock().unwrap().interval_ms
    }

    pub fn is_alive(&self) -> bool {
        self.state.lock().unwrap().alive
    }
}

impl Drop for SnapshotFeed {
    fn drop(&mut self) {
        // thread อ่าน detach อยู่ — มันจะจบเมื่อ stream ขาดเอง (EOF/err)
        // ทำแค่ตั้งสถานะตาย เพื่อ is_alive ไม่คืน true แบบลวง
        self.state.lock().unwrap().alive = false;
    }
}

fn read_loop(mut stream: LocalSocketStream, state: Arc<Mutex<FeedState>>) {
    let mut reader = FrameReader::new();
    let mut buf = [0u8; 8192];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break, // EOF — daemon ปิด
            Ok(n) => {
                let frames = match reader.feed(&buf[..n]) {
                    Ok(f) => f,
                    Err(_) => break, // ข้อมูลเสีย — stream ไม่น่าใช้ต่อ
                };
                let mut st = state.lock().unwrap();
                for frame in frames {
                    match frame {
                        Frame::Hello { version, interval_ms } => {
                            if version != PROTOCOL_VERSION {
                                st.alive = false;
                                return; // ไม่รู้จักกัน
                            }
                            st.interval_ms = interval_ms;
                        }
                        Frame::Snapshot(s) => st.latest = Some(s),
                    }
                }
            }
            Err(_) => break,
        }
    }
    state.lock().unwrap().alive = false;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{GpuStat, NetStat};
    use smallvec::SmallVec;

    #[test]
    fn frame_roundtrip() {
        let snap = Snapshot {
            ts: 1_700_000_000_123,
            cpu_total: 37.5,
            cpu_per_core: (0..8).map(|i| i as f32).collect(),
            cpu_temp: Some(61.0),
            mem_used: 8_000_000_000,
            mem_total: 16_000_000_000,
            swap_used: 500_000_000,
            disks: SmallVec::new(),
            gpus: SmallVec::from_elem(
                GpuStat {
                    name: "test gpu".into(),
                    usage_percent: Some(99.0),
                    ..Default::default()
                },
                1,
            ),
            nets: SmallVec::from_elem(
                NetStat {
                    name: "eth".into(),
                    rx_bps: 1024.0,
                    tx_bps: 512.0,
                    ..Default::default()
                },
                1,
            ),
        };
        let bytes = encode_frame(&Frame::Snapshot(snap)).unwrap();
        let mut reader = FrameReader::new();
        // ให้ทดสอบทั้งกรณีมาครึ่งเดียว
        let mid = bytes.len() / 2;
        let got = reader.feed(&bytes[..mid]).unwrap();
        assert!(got.is_empty());
        let got = reader.feed(&bytes[mid..]).unwrap();
        assert_eq!(got.len(), 1);
        assert!(matches!(got[0], Frame::Snapshot(_)));
        match &got[0] {
            Frame::Snapshot(s) => {
                assert_eq!(s.ts, 1_700_000_000_123);
                assert_eq!(s.cpu_per_core.len(), 8);
                assert_eq!(s.gpus[0].usage_percent, Some(99.0));
            }
            _ => unreachable!(),
        }
    }

    /// end-to-end: daemon จำลอง (IpcServer + ป้อน latest) ↔ SnapshotFeed จริง
    #[test]
    fn server_client_end_to_end() {
        let name = format!("sysmon-test-{}", std::process::id());
        let server = IpcServer::spawn(&name, 50).expect("เปิด server ไม่ได้");
        let slot = server.latest_slot();

        let feed = SnapshotFeed::connect(&name).expect("เชื่อมต่อไม่ได้");

        // ป้อน snapshot 2 ค่าต่างกัน — feed ต้องเห็นค่าล่าสุด
        let snap = Snapshot {
            ts: 1_000,
            cpu_total: 10.0,
            ..Default::default()
        };
        *slot.lock().unwrap() = Some(snap);
        thread::sleep(Duration::from_millis(400));
        let s = feed.latest().expect("ต้องได้ snapshot แรก");
        assert_eq!(s.ts, 1_000);
        assert_eq!(feed.interval_ms(), 50);
        assert!(feed.is_alive());

        *slot.lock().unwrap() = Some(Snapshot {
            ts: 2_000,
            cpu_total: 20.0,
            ..Default::default()
        });
        thread::sleep(Duration::from_millis(400));
        let s = feed.latest().expect("ต้องได้ snapshot ที่สอง");
        assert_eq!(s.ts, 2_000);
        assert_eq!(s.cpu_total, 20.0);
    }

    /// daemon ไม่มี → connect ต้อง fail แบบจับใจชัด ๆ (ไม่ต้อง panic)
    #[test]
    fn connect_to_missing_server_fails() {
        let name = format!("sysmon-missing-{}", std::process::id());
        assert!(SnapshotFeed::connect(&name).is_err());
    }

    /// ตัวนับ client ต้องขึ้นเมื่อมีคนต่อ และลงกลับเมื่อขาด — MonitorLoop ใช้ตัดสินลดความถี่
    /// ใช้ raw stream โดยตรงเพราะ drop มัน = ปิด pipe จริง (SnapshotFeed ถือ stream ใน reader
    /// thread จน daemon ตาย ซึ่งตรงกับการใช้งานจริงที่แอปออกทั้ง process)
    #[test]
    fn client_counter_tracks_connections() {
        let name = format!("sysmon-count-{}", std::process::id());
        let server = IpcServer::spawn(&name, 50).expect("เปิด server ไม่ได้");
        assert!(!server.has_clients());

        let ns = name.to_ns_name::<GenericNamespaced>().unwrap();
        let client = LocalSocketStream::connect(ns).expect("ต่อไม่ได้");
        for _ in 0..40 {
            if server.has_clients() {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        assert!(server.has_clients());

        // daemon รู้ตัวว่า client หายตอนพยายามเขียน — ปิดฝั่ง client แล้วป้อนค่าใหม่บังคับเขียน
        drop(client);
        *server.latest_slot().lock().unwrap() = Some(Snapshot {
            ts: 42_000,
            ..Default::default()
        });
        for _ in 0..60 {
            if !server.has_clients() {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        assert!(!server.has_clients());
    }
}

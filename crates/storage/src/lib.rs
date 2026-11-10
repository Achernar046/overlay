//! Storage — เก็บประวัติ metrics ลง SQLite (Phase 3)
//!
//! ตาม plan:
//! - WAL + synchronous=NORMAL
//! - เขียนเป็น transaction เดียวทุกช่วง flush (daemon เป็นผู้เรียก)
//! - rollup 1 นาที / 1 ชั่วโมง + retention ลบข้อมูลเก่า
//! - schema_version อยู่ในตาราง meta ตั้งแต่วันแรก
//!
//! หมายเหตุการออกแบบ: plan เดิมร่าง schema EAV (metric TEXT ต่อแถว) ซึ่งกินพื้นที่และช้า
//! ที่นี่ใช้ตารางกว้างและตารางข้อมูลลูกแยก gpu/disk/net; ts ทั้งหมดเป็น unix milliseconds

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::{params, Connection, Result};

use sysmon_core::Snapshot;

pub const SCHEMA_VERSION: i32 = 1;

/// ค่า default ของ retention ตาม plan (raw 24 ชม. → 1m 30 วัน → 1h 1 ปี)
pub const RETENTION_1M_DAYS: u64 = 30;
pub const RETENTION_1H_DAYS: u64 = 365;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);

CREATE TABLE IF NOT EXISTS metrics_raw (
    ts        INTEGER PRIMARY KEY,
    cpu_total REAL,
    cpu_temp  REAL,
    mem_used  INTEGER,
    mem_total INTEGER,
    swap_used INTEGER
);

CREATE TABLE IF NOT EXISTS metrics_gpu (
    ts        INTEGER NOT NULL,
    gpu_idx   INTEGER NOT NULL,
    name      TEXT    NOT NULL,
    usage     REAL,
    mem_used  INTEGER,
    mem_total INTEGER,
    temp      REAL
);
CREATE INDEX IF NOT EXISTS idx_gpu_ts ON metrics_gpu(ts);

CREATE TABLE IF NOT EXISTS metrics_disk (
    ts    INTEGER NOT NULL,
    mount TEXT    NOT NULL,
    used  INTEGER NOT NULL,
    total INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_disk_ts ON metrics_disk(ts);

CREATE TABLE IF NOT EXISTS metrics_net (
    ts       INTEGER NOT NULL,
    name     TEXT    NOT NULL,
    rx_bps   REAL,
    tx_bps   REAL,
    rx_total INTEGER,
    tx_total INTEGER
);
CREATE INDEX IF NOT EXISTS idx_net_ts ON metrics_net(ts);

-- rollup (ค่าเฉลี่ยของ min/max/avg ใน bucket)
CREATE TABLE IF NOT EXISTS metrics_1m (
    ts        INTEGER PRIMARY KEY,
    samples   INTEGER NOT NULL,
    cpu_avg   REAL, cpu_min REAL, cpu_max REAL,
    cpu_temp  REAL,
    mem_used  INTEGER,
    swap_used INTEGER,
    net_rx    REAL,
    net_tx    REAL
);

CREATE TABLE IF NOT EXISTS metrics_1h (
    ts        INTEGER PRIMARY KEY,
    samples   INTEGER NOT NULL,
    cpu_avg   REAL, cpu_min REAL, cpu_max REAL,
    cpu_temp  REAL,
    mem_used  INTEGER,
    swap_used INTEGER,
    net_rx    REAL,
    net_tx    REAL
);
"#;

pub struct Storage {
    conn: Connection,
}

/// เลข bucket เริ่มต้นนาที (ms) ที่ ts ตกอยู่
pub fn minute_bucket(ts_ms: i64) -> i64 {
    ts_ms - ts_ms % 60_000
}

/// เลข bucket เริ่มต้นชั่วโมง (ms) ที่ ts ตกอยู่
pub fn hour_bucket(ts_ms: i64) -> i64 {
    ts_ms - ts_ms % 3_600_000
}

/// สรุปค่าหนึ่ง bucket — ไม่มีข้อมูล = None
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Rollup {
    pub samples: i64,
    pub cpu_avg: Option<f64>,
    pub cpu_min: Option<f64>,
    pub cpu_max: Option<f64>,
    pub cpu_temp: Option<f64>,
    pub mem_used: Option<f64>,
    pub swap_used: Option<f64>,
    pub net_rx: Option<f64>,
    pub net_tx: Option<f64>,
}

impl Storage {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        Self::init(conn, true)
    }

    /// ฐานข้อมูลในหน่วยความจำสำหรับทดสอบ (WAL ไม่มีความหมายกับ :memory:)
    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::init(conn, false)
    }

    fn init(conn: Connection, wal: bool) -> Result<Self> {
        if wal {
            conn.pragma_update(None, "journal_mode", "WAL")?;
            conn.pragma_update(None, "synchronous", "NORMAL")?;
        }
        conn.execute_batch(SCHEMA)?;
        conn.execute(
            "INSERT INTO schema_meta(key, value) VALUES ('schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![SCHEMA_VERSION.to_string()],
        )?;
        Ok(Self { conn })
    }

    /// เขียน batch ของ snapshots เป็นหนึ่ง transaction — คืนจำนวน snapshot ที่เขียน
    pub fn insert_snapshots(&mut self, snaps: &[Snapshot]) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let mut inserted = 0usize;
        {
            let mut raw = tx.prepare_cached(
                "INSERT OR REPLACE INTO metrics_raw(ts, cpu_total, cpu_temp, mem_used, mem_total, swap_used)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            let mut gpu = tx.prepare_cached(
                "INSERT INTO metrics_gpu(ts, gpu_idx, name, usage, mem_used, mem_total, temp)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            let mut disk = tx.prepare_cached(
                "INSERT INTO metrics_disk(ts, mount, used, total) VALUES (?1, ?2, ?3, ?4)",
            )?;
            let mut net = tx.prepare_cached(
                "INSERT INTO metrics_net(ts, name, rx_bps, tx_bps, rx_total, tx_total)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;

            for s in snaps {
                raw.execute(params![
                    s.ts, s.cpu_total, s.cpu_temp, s.mem_used as i64, s.mem_total as i64, s.swap_used as i64
                ])?;
                inserted += 1;
                for (i, g) in s.gpus.iter().enumerate() {
                    gpu.execute(params![
                        s.ts, i as i64, g.name, g.usage_percent,
                        g.mem_used_mb.map(|v| v as i64), g.mem_total_mb.map(|v| v as i64), g.temp_c
                    ])?;
                }
                for d in &s.disks {
                    disk.execute(params![s.ts, d.mount, d.used_bytes as i64, d.total_bytes as i64])?;
                }
                for n in &s.nets {
                    net.execute(params![
                        s.ts, n.name, n.rx_bps, n.tx_bps, n.rx_total as i64, n.tx_total as i64
                    ])?;
                }
            }
        }
        tx.commit()?;
        Ok(inserted)
    }

    /// rollup หนึ่ง bucket นาทีจาก metrics_raw — idempotent (UPSERT)
    /// ข้ามถ้า bucket ว่าง (ไม่เขียนแถว samples=0 ให้เป็นขยะ)
    pub fn rollup_minute_bucket(&self, bucket_start_ms: i64) -> Result<()> {
        let end = bucket_start_ms + 60_000;
        let r = self.aggregate_raw(bucket_start_ms, end)?;
        if r.samples == 0 {
            return Ok(());
        }
        self.upsert_rollup("metrics_1m", bucket_start_ms, &r)
    }

    /// rollup หนึ่ง bucket ชั่วโมงจาก metrics_1m — idempotent (UPSERT)
    pub fn rollup_hour_bucket(&self, bucket_start_ms: i64) -> Result<()> {
        let end = bucket_start_ms + 3_600_000;

        let mut stmt = self.conn.prepare(
            "SELECT samples, cpu_avg, cpu_min, cpu_max, cpu_temp, mem_used, swap_used, net_rx, net_tx
             FROM metrics_1m WHERE ts >= ?1 AND ts < ?2",
        )?;
        let rows = stmt.query_map(params![bucket_start_ms, end], |row| {
            Ok(Rollup {
                samples: row.get(0)?,
                cpu_avg: row.get(1)?,
                cpu_min: row.get(2)?,
                cpu_max: row.get(3)?,
                cpu_temp: row.get(4)?,
                mem_used: row.get(5)?,
                swap_used: row.get(6)?,
                net_rx: row.get(7)?,
                net_tx: row.get(8)?,
            })
        })?;

        let mut total_samples = 0i64;
        let mut cpu_vals: Vec<f64> = Vec::new();
        let mut mins: Vec<f64> = Vec::new();
        let mut maxes: Vec<f64> = Vec::new();
        let mut temps: Vec<f64> = Vec::new();
        let mut mems: Vec<f64> = Vec::new();
        let mut swaps: Vec<f64> = Vec::new();
        let mut rxs: Vec<f64> = Vec::new();
        let mut txs: Vec<f64> = Vec::new();

        for row in rows {
            let r = row?;
            total_samples += r.samples;
            if let Some(v) = r.cpu_avg {
                cpu_vals.push(v);
            }
            if let Some(v) = r.cpu_min {
                mins.push(v);
            }
            if let Some(v) = r.cpu_max {
                maxes.push(v);
            }
            if let Some(v) = r.cpu_temp {
                temps.push(v);
            }
            if let Some(v) = r.mem_used {
                mems.push(v);
            }
            if let Some(v) = r.swap_used {
                swaps.push(v);
            }
            if let Some(v) = r.net_rx {
                rxs.push(v);
            }
            if let Some(v) = r.net_tx {
                txs.push(v);
            }
        }

        let r = Rollup {
            samples: total_samples,
            cpu_avg: avg(&cpu_vals),
            cpu_min: mins.iter().copied().reduce(f64::min),
            cpu_max: maxes.iter().copied().reduce(f64::max),
            cpu_temp: avg(&temps),
            mem_used: avg(&mems),
            swap_used: avg(&swaps),
            net_rx: avg(&rxs),
            net_tx: avg(&txs),
        };
        if r.samples == 0 {
            return Ok(());
        }
        self.upsert_rollup("metrics_1h", bucket_start_ms, &r)
    }

    /// ลบข้อมูลเก่ากว่า retention ตาม plan — คืนจำนวนแถวที่ลบ
    pub fn apply_retention(&self, now_ms: i64, raw_hours: u64) -> Result<usize> {
        let raw_cutoff = now_ms - (raw_hours as i64) * 3_600_000;
        let m1_cutoff = now_ms - RETENTION_1M_DAYS as i64 * 86_400_000;
        let h1_cutoff = now_ms - RETENTION_1H_DAYS as i64 * 86_400_000;

        let mut total = 0usize;
        for (sql, cutoff) in [
            ("DELETE FROM metrics_raw WHERE ts < ?1", raw_cutoff),
            ("DELETE FROM metrics_gpu  WHERE ts < ?1", raw_cutoff),
            ("DELETE FROM metrics_disk WHERE ts < ?1", raw_cutoff),
            ("DELETE FROM metrics_net  WHERE ts < ?1", raw_cutoff),
            ("DELETE FROM metrics_1m   WHERE ts < ?1", m1_cutoff),
            ("DELETE FROM metrics_1h   WHERE ts < ?1", h1_cutoff),
        ] {
            total += self.conn.execute(sql, params![cutoff])?;
        }
        Ok(total)
    }

    /// รวม metrics_raw + metrics_net ใน [start, end) เป็น Rollup หนึ่งอัน
    fn aggregate_raw(&self, start: i64, end: i64) -> Result<Rollup> {
        let mut cpus: Vec<f64> = Vec::new();
        let mut temps: Vec<f64> = Vec::new();
        let mut mems: Vec<f64> = Vec::new();
        let mut swaps: Vec<f64> = Vec::new();
        let mut samples = 0i64;

        {
            let mut stmt = self.conn.prepare(
                "SELECT cpu_total, cpu_temp, mem_used, swap_used
                 FROM metrics_raw WHERE ts >= ?1 AND ts < ?2",
            )?;
            let rows = stmt.query_map(params![start, end], |row| {
                Ok((
                    row.get::<_, Option<f64>>(0)?,
                    row.get::<_, Option<f64>>(1)?,
                    row.get::<_, Option<f64>>(2)?,
                    row.get::<_, Option<f64>>(3)?,
                ))
            })?;
            for row in rows {
                let (cpu, temp, mem, swap) = row?;
                samples += 1;
                if let Some(v) = cpu {
                    cpus.push(v);
                }
                if let Some(v) = temp {
                    temps.push(v);
                }
                if let Some(v) = mem {
                    mems.push(v);
                }
                if let Some(v) = swap {
                    swaps.push(v);
                }
            }
        }

        // ต่อ timestamp: ผลรวมทุก interface ก่อน แล้วเฉลี่ยตาม bucket
        let mut stmt = self.conn.prepare(
            "SELECT ts, rx_bps, tx_bps FROM metrics_net WHERE ts >= ?1 AND ts < ?2",
        )?;
        let rows = stmt.query_map(params![start, end], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<f64>>(1)?,
                row.get::<_, Option<f64>>(2)?,
            ))
        })?;
        let mut by_ts: BTreeMap<i64, (f64, f64)> = BTreeMap::new();
        for row in rows {
            let (ts, rx, tx) = row?;
            let entry = by_ts.entry(ts).or_insert((0.0, 0.0));
            entry.0 += rx.unwrap_or(0.0);
            entry.1 += tx.unwrap_or(0.0);
        }

        let mut rx_vals: Vec<f64> = Vec::new();
        let mut tx_vals: Vec<f64> = Vec::new();
        for (rx, tx) in by_ts.values() {
            if *rx > 0.0 {
                rx_vals.push(*rx);
            }
            if *tx > 0.0 {
                tx_vals.push(*tx);
            }
        }

        Ok(Rollup {
            samples,
            cpu_avg: avg(&cpus),
            cpu_min: cpus.iter().copied().reduce(f64::min),
            cpu_max: cpus.iter().copied().reduce(f64::max),
            cpu_temp: avg(&temps),
            mem_used: avg(&mems),
            swap_used: avg(&swaps),
            net_rx: avg(&rx_vals),
            net_tx: avg(&tx_vals),
        })
    }

    fn upsert_rollup(&self, table: &str, ts: i64, r: &Rollup) -> Result<()> {
        let sql = format!(
            "INSERT OR REPLACE INTO {table}
             (ts, samples, cpu_avg, cpu_min, cpu_max, cpu_temp, mem_used, swap_used, net_rx, net_tx)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"
        );
        self.conn.execute(
            &sql,
            params![
                ts,
                r.samples,
                r.cpu_avg,
                r.cpu_min,
                r.cpu_max,
                r.cpu_temp,
                r.mem_used,
                r.swap_used,
                r.net_rx,
                r.net_tx
            ],
        )?;
        Ok(())
    }
}

fn avg(v: &[f64]) -> Option<f64> {
    if v.is_empty() {
        None
    } else {
        Some(v.iter().sum::<f64>() / v.len() as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smallvec::SmallVec;
    use sysmon_core::{DiskStat, GpuStat, NetStat, Snapshot};

    /// สร้าง snapshot หลอด้วยค่า metric ชัดเจนเพื่อ assert คำนวณด้วยมือ
    fn snap(ts: i64, cpu: f32, temp: f32, mem: u64, swap: u64) -> Snapshot {
        Snapshot {
            ts,
            cpu_total: cpu,
            cpu_per_core: SmallVec::from_elem(cpu, 4),
            cpu_temp: Some(temp),
            mem_used: mem,
            mem_total: 8_000_000_000,
            swap_used: swap,
            disks: SmallVec::from_vec(vec![DiskStat {
                name: "sda".into(),
                mount: "C:\\".into(),
                total_bytes: 1_000,
                used_bytes: 500,
            }]),
            gpus: SmallVec::from_vec(vec![GpuStat {
                name: "RTX".into(),
                usage_percent: Some(40.0),
                mem_used_mb: Some(100),
                mem_total_mb: Some(1000),
                temp_c: Some(60.0),
            }]),
            nets: SmallVec::from_vec(vec![
                NetStat { name: "eth0".into(), rx_bps: 10.0, tx_bps: 2.0, ..Default::default() },
                NetStat { name: "wifi".into(), rx_bps: 20.0, tx_bps: 4.0, ..Default::default() },
            ]),
        }
    }

    #[test]
    fn schema_version_written() {
        let st = Storage::open_memory().unwrap();
        let v: String = st
            .conn
            .query_row("SELECT value FROM schema_meta WHERE key = 'schema_version'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, "1");
    }

    #[test]
    fn insert_and_rollup_minute() {
        let mut st = Storage::open_memory().unwrap();
        // ทั้งสองแถวอยู่ bucket นาทีเดียวกัน (ts % 60000 คงที่ใน bucket 0)
        st.insert_snapshots(&[snap(1000, 10.0, 40.0, 1_000_000, 0), snap(2000, 30.0, 50.0, 3_000_000, 100)])
            .unwrap();

        st.rollup_minute_bucket(minute_bucket(1000)).unwrap();

        let (cpu_avg, cpu_min, cpu_max, mem, rx): (f64, f64, f64, f64, f64) = st
            .conn
            .query_row(
                "SELECT cpu_avg, cpu_min, cpu_max, mem_used, net_rx FROM metrics_1m",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(cpu_avg, 20.0);
        assert_eq!(cpu_min, 10.0);
        assert_eq!(cpu_max, 30.0);
        assert_eq!(mem, 2_000_000.0);
        // rx: ผลรวมทุก interface ต่อ ts = 30 แล้วเฉลี่ย = 30
        assert_eq!(rx, 30.0);
    }

    #[test]
    fn rollup_hour_from_minute_buckets() {
        let mut st = Storage::open_memory().unwrap();
        // bucket นาที 2 อัน ในชั่วโมงเดียวกัน
        st.insert_snapshots(&[snap(1_000_000, 10.0, 40.0, 1_000, 0), snap(2_000_000, 20.0, 50.0, 2_000, 0)])
            .unwrap();
        st.rollup_minute_bucket(minute_bucket(1_000_000)).unwrap();
        st.rollup_minute_bucket(minute_bucket(2_000_000)).unwrap();
        st.rollup_hour_bucket(hour_bucket(1_000_000)).unwrap();

        let (cpu_avg, cpu_max, samples): (f64, f64, i64) = st
            .conn
            .query_row("SELECT cpu_avg, cpu_max, samples FROM metrics_1h", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap();
        assert_eq!(cpu_avg, 15.0);
        assert_eq!(cpu_max, 20.0);
        assert_eq!(samples, 2);
    }

    #[test]
    fn retention_deletes_old_rows() {
        let mut st = Storage::open_memory().unwrap();
        let now = 3_000_000_000i64; // ~ ผ่านไปมากพอ
        let day_ms = 86_400_000i64;

        st.insert_snapshots(&[
            snap(now, 1.0, 40.0, 1, 1),
            snap(now - day_ms * 2, 2.0, 41.0, 2, 2), // เก่ากว่า raw 24 ชม.
        ])
        .unwrap();
        st.rollup_minute_bucket(minute_bucket(now - day_ms * 40)).unwrap();
        st.rollup_hour_bucket(hour_bucket(now - day_ms * 500)).unwrap();

        let deleted = st.apply_retention(now, 24).unwrap();
        assert!(deleted > 0);

        let raw_left: i64 = st.conn.query_row("SELECT COUNT(*) FROM metrics_raw", [], |r| r.get(0)).unwrap();
        let m1_left: i64 = st.conn.query_row("SELECT COUNT(*) FROM metrics_1m", [], |r| r.get(0)).unwrap();
        let h1_left: i64 = st.conn.query_row("SELECT COUNT(*) FROM metrics_1h", [], |r| r.get(0)).unwrap();
        assert_eq!(raw_left, 1);
        assert_eq!(m1_left, 0); // เก่ากว่า 30 วัน
        assert_eq!(h1_left, 0); // เก่ากว่า 365 วัน
    }

    #[test]
    fn child_tables_filled_per_snapshot() {
        let mut st = Storage::open_memory().unwrap();
        st.insert_snapshots(&[snap(1000, 5.0, 30.0, 1, 1)]).unwrap();

        let gpu_rows: i64 = st.conn.query_row("SELECT COUNT(*) FROM metrics_gpu", [], |r| r.get(0)).unwrap();
        let disk_rows: i64 = st.conn.query_row("SELECT COUNT(*) FROM metrics_disk", [], |r| r.get(0)).unwrap();
        let net_rows: i64 = st.conn.query_row("SELECT COUNT(*) FROM metrics_net", [], |r| r.get(0)).unwrap();
        assert_eq!((gpu_rows, disk_rows, net_rows), (1, 1, 2));
    }
}

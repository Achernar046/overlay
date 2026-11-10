"""ตรวจ DB ของ sysmon อย่างรวดเร็ว (dev tool) — รันที่โฟลเดอร์โปรเจกต์: python scripts/check_db.py [db_path]"""
import sqlite3
import sys
from pathlib import Path

path = sys.argv[1] if len(sys.argv) > 1 else 'sysmon.db'
print('exists:', Path(path).exists())
c = sqlite3.connect(path)
print('schema_version:', c.execute("SELECT value FROM schema_meta WHERE key='schema_version'").fetchone())
for t in ['metrics_raw', 'metrics_gpu', 'metrics_disk', 'metrics_net', 'metrics_1m', 'metrics_1h']:
    print(t, '=', c.execute(f'SELECT COUNT(*) FROM {t}').fetchone()[0])
print('raw newest:', c.execute('SELECT ts, cpu_total, mem_used FROM metrics_raw ORDER BY ts DESC LIMIT 3').fetchall())
print('1m:', c.execute('SELECT ts, samples, round(cpu_avg,1), round(cpu_max,1) FROM metrics_1m ORDER BY ts DESC LIMIT 3').fetchall())
print('journal_mode:', c.execute('PRAGMA journal_mode').fetchone())

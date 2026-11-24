//! Standalone replay stores. One writer; the benchmark supplies its private directory.
use crate::tree::{Arena, Edits, Entry, Rows};
use rusqlite::{Connection, params};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const MAX_RUNS: usize = 4;

struct Pages {
    path: PathBuf,
    arena: Arena,
    roots: Vec<u64>,
    runs: bool,
    epoch: u64,
}
impl Pages {
    fn create(path: &Path, runs: bool) -> Result<Self> {
        let store = Self {
            path: path.into(),
            arena: Arena::new(
                OpenOptions::new()
                    .create_new(true)
                    .read(true)
                    .append(true)
                    .open(path.join("arena-0"))?,
            )?,
            roots: vec![],
            runs,
            epoch: 0,
        };
        File::open(path)?.sync_all()?;
        Ok(store)
    }
    fn publish(&self) -> Result<()> {
        let start = std::time::Instant::now();
        self.arena.sync()?;
        let arena_synced = std::time::Instant::now();
        let mut f = File::create(self.path.join("pending"))?;
        f.write_all(b"CXREPLAY")?;
        f.write_all(&self.epoch.to_le_bytes())?;
        f.write_all(&self.arena.len().to_le_bytes())?;
        for root in &self.roots {
            f.write_all(&root.to_le_bytes())?
        }
        f.sync_all()?;
        let selector_synced = std::time::Instant::now();
        fs::rename(self.path.join("pending"), self.path.join("current"))?;
        File::open(&self.path)?.sync_all()?;
        if std::env::var_os("CX_REPLAY_SYNC_TIMES").is_some() {
            eprintln!(
                "sync_us {} {} {}",
                (arena_synced - start).as_micros(),
                (selector_synced - arena_synced).as_micros(),
                selector_synced.elapsed().as_micros()
            );
        }
        Ok(())
    }
    fn reopen(&mut self) -> Result<()> {
        let data = fs::read(self.path.join("current"))?;
        if data.len() < 24 || data.len() % 8 != 0 || &data[..8] != b"CXREPLAY" {
            return Err("invalid selector".into());
        }
        self.epoch = u64::from_le_bytes(data[8..16].try_into()?);
        let f = OpenOptions::new()
            .read(true)
            .append(true)
            .open(self.path.join(format!("arena-{}", self.epoch)))?;
        let len = u64::from_le_bytes(data[16..24].try_into()?);
        let physical = f.metadata()?.len();
        if physical < len {
            return Err("short arena".into());
        }
        if physical > len {
            f.set_len(len)?;
        }
        self.arena = Arena::new(f)?;
        self.roots = data[24..]
            .chunks_exact(8)
            .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
            .collect();
        Ok(())
    }
    fn scan(&self, key: &[u8], limit: usize) -> Result<Vec<Entry>> {
        if !self.runs {
            return Ok(self.arena.scan(self.roots.first().copied(), key, limit)?);
        }
        // Each run has its own seek cursor. Empty values are tombstones; the
        // newest run wins before limit and ordering are applied.
        let batch = limit.clamp(1, 64);
        let mut cursors: Vec<_> = self
            .roots
            .iter()
            .map(|&id| {
                self.arena
                    .scan(Some(id), key, batch)
                    .map(|v| v.into_iter().peekable())
            })
            .collect::<io::Result<_>>()?;
        let mut out = Vec::new();
        while out.len() < limit {
            let Some(min) = cursors
                .iter_mut()
                .filter_map(|c| c.peek().map(|e| &e.0))
                .min()
                .cloned()
            else {
                break;
            };
            let mut value = Vec::new();
            for (idx, c) in cursors.iter_mut().enumerate() {
                if c.peek().is_some_and(|e| e.0 == min) {
                    value = c.next().unwrap().1;
                    if c.peek().is_none() {
                        let mut next = min.clone();
                        next.push(0);
                        *c = self
                            .arena
                            .scan(Some(self.roots[idx]), &next, batch)?
                            .into_iter()
                            .peekable();
                    }
                }
            }
            if let Some((_, body)) = value.split_first() {
                out.push((min, body.to_vec()))
            }
        }
        Ok(out)
    }
    fn tag(v: &[u8]) -> Vec<u8> {
        let mut x = Vec::with_capacity(v.len() + 1);
        x.push(1);
        x.extend(v);
        x
    }
    fn bulk(&mut self, rows: Rows) -> Result<()> {
        let rows = if self.runs {
            rows.into_iter().map(|(k, v)| (k, Self::tag(&v))).collect()
        } else {
            rows
        };
        self.roots = self.arena.bulk(rows)?.into_iter().collect();
        self.publish()
    }
    fn edit(&mut self, edits: &Edits) -> Result<()> {
        if !self.runs {
            self.roots = self
                .arena
                .update(self.roots.first().copied(), edits)?
                .into_iter()
                .collect();
        } else if self.roots.len() >= MAX_RUNS {
            let mut rows: Rows = self.scan(b"", usize::MAX)?.into_iter().collect();
            apply(&mut rows, edits);
            return self.replace(rows);
        } else {
            let rows = edits
                .iter()
                .map(|(k, v)| (k.clone(), v.as_deref().map(Self::tag).unwrap_or_default()))
                .collect();
            if let Some(id) = self.arena.bulk(rows)? {
                self.roots.push(id)
            }
        }
        self.publish()
    }
    fn replace(&mut self, rows: Rows) -> Result<()> {
        let old = self.epoch;
        self.epoch += 1;
        self.arena = Arena::new(
            OpenOptions::new()
                .read(true)
                .append(true)
                .create_new(true)
                .open(self.path.join(format!("arena-{}", self.epoch)))?,
        )?;
        File::open(&self.path)?.sync_all()?;
        self.bulk(rows)?;
        fs::remove_file(self.path.join(format!("arena-{old}")))?;
        File::open(&self.path)?.sync_all()?;
        Ok(())
    }
}
pub fn apply(rows: &mut Rows, edits: &Edits) {
    for (k, v) in edits {
        if let Some(v) = v {
            rows.insert(k.clone(), v.clone());
        } else {
            rows.remove(k);
        }
    }
}
#[derive(PartialEq, Eq)]
enum SqlLayout {
    Rows,
    BatchedRows,
    Blocks,
}

struct Sql {
    layout: SqlLayout,
    conn: Connection,
    path: PathBuf,
}
fn connect(path: &Path, large_pages: bool) -> Result<Connection> {
    let c = Connection::open(path)?;
    if large_pages {
        c.execute_batch("PRAGMA page_size=16384")?;
    }
    c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA fullfsync=ON; PRAGMA checkpoint_fullfsync=ON; PRAGMA wal_autocheckpoint=0; PRAGMA cache_size=-2048;")?;
    Ok(c)
}
fn insert_batches(conn: &mut Connection, rows: Rows) -> Result<()> {
    use rusqlite::types::Value;
    const BATCH: usize = 128;
    let tx = conn.transaction()?;
    tx.execute_batch("CREATE TABLE rows(k BLOB PRIMARY KEY,v BLOB NOT NULL) WITHOUT ROWID")?;
    let sql = format!("INSERT INTO rows VALUES {}", vec!["(?,?)"; BATCH].join(","));
    {
        let mut stmt = tx.prepare(&sql)?;
        let mut parameters = Vec::with_capacity(2 * BATCH);
        for (k, v) in rows {
            parameters.push(Value::Blob(k));
            parameters.push(Value::Blob(v));
            if parameters.len() == 2 * BATCH {
                stmt.execute(rusqlite::params_from_iter(&parameters))?;
                parameters.clear();
            }
        }
        if !parameters.is_empty() {
            let tail = format!(
                "INSERT INTO rows VALUES {}",
                vec!["(?,?)"; parameters.len() / 2].join(",")
            );
            tx.execute(&tail, rusqlite::params_from_iter(&parameters))?;
        }
    }
    tx.commit()?;
    Ok(())
}

enum Engine {
    Packed(Pages),
    Sql(Sql),
}
pub struct Store(Engine);
impl Store {
    pub fn create(path: &Path, mode: &str) -> Result<Self> {
        fs::create_dir(path)?;
        Ok(Self(match mode {
            "cow" | "runs" => Engine::Packed(Pages::create(path, mode == "runs")?),
            "sqlite" | "sqlite-blocks" | "sqlite-batch" => {
                let path = path.join("rows.db");
                let layout = match mode {
                    "sqlite-blocks" => SqlLayout::Blocks,
                    "sqlite-batch" => SqlLayout::BatchedRows,
                    _ => SqlLayout::Rows,
                };
                let conn = connect(&path, layout != SqlLayout::Rows)?;
                if layout == SqlLayout::Rows {
                    conn.execute_batch(
                        "CREATE TABLE rows(k BLOB PRIMARY KEY,v BLOB NOT NULL) WITHOUT ROWID",
                    )?;
                }
                Engine::Sql(Sql { conn, path, layout })
            }
            _ => return Err("unknown backend".into()),
        }))
    }
    pub fn bulk(&mut self, rows: Rows) -> Result<()> {
        match &mut self.0 {
            Engine::Packed(p) => p.bulk(rows),
            Engine::Sql(s) if s.layout == SqlLayout::BatchedRows => {
                insert_batches(&mut s.conn, rows)
            }
            Engine::Sql(s) if s.layout == SqlLayout::Blocks => {
                crate::sql_blocks::bulk(&mut s.conn, rows)
            }
            Engine::Sql(s) => {
                let t = s.conn.transaction()?;
                {
                    let mut q = t.prepare("INSERT INTO rows VALUES(?1,?2)")?;
                    for (k, v) in rows {
                        q.execute(params![k, v])?;
                    }
                }
                t.commit()?;
                Ok(())
            }
        }
    }
    pub fn edit(&mut self, edits: &Edits) -> Result<()> {
        match &mut self.0 {
            Engine::Packed(p) => p.edit(edits),
            Engine::Sql(s) if s.layout == SqlLayout::Blocks => {
                crate::sql_blocks::edit(&mut s.conn, edits)
            }
            Engine::Sql(s) => {
                let t = s.conn.transaction()?;
                {
                    let mut ins = t.prepare("INSERT OR REPLACE INTO rows VALUES(?1,?2)")?;
                    let mut del = t.prepare("DELETE FROM rows WHERE k=?1")?;
                    for (k, v) in edits {
                        if let Some(v) = v {
                            ins.execute(params![k, v])?;
                        } else {
                            del.execute([k])?;
                        }
                    }
                }
                t.commit()?;
                Ok(())
            }
        }
    }
    pub fn scan(&self, key: &[u8], limit: usize) -> Result<Vec<Entry>> {
        match &self.0 {
            Engine::Packed(p) => p.scan(key, limit),
            Engine::Sql(s) if s.layout == SqlLayout::Blocks => {
                crate::sql_blocks::scan(&s.conn, key, limit)
            }
            Engine::Sql(s) => {
                let mut q = s
                    .conn
                    .prepare_cached("SELECT k,v FROM rows WHERE k>=?1 ORDER BY k LIMIT ?2")?;
                Ok(q.query_map(
                    params![key, i64::try_from(limit).unwrap_or(i64::MAX)],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?
                .collect::<rusqlite::Result<_>>()?)
            }
        }
    }
    pub fn reopen(&mut self) -> Result<()> {
        match &mut self.0 {
            Engine::Packed(p) => p.reopen(),
            Engine::Sql(s) => {
                s.conn = connect(&s.path, false)?;
                Ok(())
            }
        }
    }
    pub fn compact(&mut self) -> Result<()> {
        match &mut self.0 {
            Engine::Packed(p) => {
                let rows: BTreeMap<_, _> = p.scan(b"", usize::MAX)?.into_iter().collect();
                p.replace(rows)
            }
            Engine::Sql(s) => {
                s.conn.execute_batch(
                    "PRAGMA wal_checkpoint(TRUNCATE); VACUUM; PRAGMA wal_checkpoint(TRUNCATE);",
                )?;
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpublished_tail_is_discarded_and_old_pin_survives_compaction() -> Result<()> {
        let path = std::env::temp_dir().join(format!("cx-pages-recovery-{}", std::process::id()));
        fs::create_dir(&path)?;
        let mut store = Pages::create(&path, false)?;
        let original = Rows::from([(b"k".to_vec(), b"first".to_vec())]);
        store.bulk(original.clone())?;
        let old_root = store.roots[0];
        let old_pin = Arena::new(File::open(path.join("arena-0"))?)?;
        let sealed = store.arena.len();
        let edits = Edits::from([(b"k".to_vec(), Some(b"second".to_vec()))]);
        store.arena.update(Some(old_root), &edits)?;
        store.arena.sync()?;
        assert!(store.arena.len() > sealed);
        store.reopen()?;
        assert_eq!(store.arena.len(), sealed);
        assert!(store.scan(b"", 2)?.into_iter().eq(original.clone()));
        store.edit(&edits)?;
        let current: Rows = store.scan(b"", 2)?.into_iter().collect();
        store.replace(current.clone())?;
        store.reopen()?;
        assert!(!path.join("arena-0").exists());
        assert!(
            old_pin
                .scan(Some(old_root), b"", 2)?
                .into_iter()
                .eq(original)
        );
        assert!(store.scan(b"", 2)?.into_iter().eq(current));
        fs::remove_dir_all(path)?;
        Ok(())
    }
}

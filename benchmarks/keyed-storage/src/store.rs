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
const SELECTOR_MAGIC: &[u8; 8] = b"CXREPLAY";

struct RunCursor {
    root: u64,
    more: bool,
    entries: std::iter::Peekable<std::vec::IntoIter<Entry>>,
}

impl RunCursor {
    fn open(arena: &Arena, root: u64, key: &[u8], batch: usize) -> io::Result<Self> {
        let entries = arena.scan(Some(root), key, batch)?;
        Ok(Self {
            root,
            more: entries.len() == batch,
            entries: entries.into_iter().peekable(),
        })
    }

    fn next_visible(
        cursors: &mut [Self],
        arena: &Arena,
        batch: usize,
    ) -> io::Result<Option<Entry>> {
        while let Some(min) = Self::first_key(cursors) {
            if let Some(value) = Self::newest_value(cursors, arena, &min, batch)? {
                return Ok(Some((min, value)));
            }
        }
        Ok(None)
    }

    fn first_key(cursors: &mut [Self]) -> Option<Vec<u8>> {
        cursors.iter_mut().filter_map(|cursor| cursor.entries.peek().map(|entry| &entry.0))
            .min().cloned()
    }

    fn newest_value(cursors: &mut [Self], arena: &Arena, key: &[u8], batch: usize) -> io::Result<Option<Vec<u8>>> {
        let mut value = Vec::new();
        for cursor in cursors {
            if let Some(newer) = cursor.take(arena, key, batch)? {
                value = newer;
            }
        }
        if value.is_empty() {
            return Ok(None);
        }
        value.remove(0);
        Ok(Some(value))
    }

    fn take(&mut self, arena: &Arena, key: &[u8], batch: usize) -> io::Result<Option<Vec<u8>>> {
        let Some((_, value)) = self.entries.next_if(|entry| entry.0.as_slice() == key) else {
            return Ok(None);
        };
        if self.more && self.entries.peek().is_none() {
            let mut next = key.to_vec();
            next.push(0);
            *self = Self::open(arena, self.root, &next, batch)?;
        }
        Ok(Some(value))
    }
}

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
            arena: Self::create_arena(path, 0)?,
            roots: vec![],
            runs,
            epoch: 0,
        };
        File::open(path)?.sync_all()?;
        Ok(store)
    }
    fn create_arena(path: &Path, epoch: u64) -> io::Result<Arena> {
        let file = OpenOptions::new()
            .read(true)
            .append(true)
            .create_new(true)
            .open(path.join(format!("arena-{epoch}")))?;
        Arena::new(file)
    }
    fn publish(&self) -> Result<()> {
        let start = std::time::Instant::now();
        self.arena.sync()?;
        let arena_synced = std::time::Instant::now();
        let mut f = File::create(self.path.join("pending"))?;
        f.write_all(SELECTOR_MAGIC)?;
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
        let (magic, words) = data.split_at_checked(8).ok_or("invalid selector")?;
        let (words, tail) = words.as_chunks::<8>();
        let [epoch, len, roots @ ..] = words else {
            return Err("invalid selector".into());
        };
        if magic != SELECTOR_MAGIC || !tail.is_empty() {
            return Err("invalid selector".into());
        }
        let epoch = u64::from_le_bytes(*epoch);
        let len = u64::from_le_bytes(*len);
        let f = OpenOptions::new()
            .read(true)
            .append(true)
            .open(self.path.join(format!("arena-{epoch}")))?;
        let physical = f.metadata()?.len();
        if physical < len {
            return Err("short arena".into());
        }
        if physical > len {
            f.set_len(len)?;
        }
        self.arena = Arena::new(f)?;
        self.epoch = epoch;
        self.roots = roots.iter().copied().map(u64::from_le_bytes).collect();
        Ok(())
    }
    fn scan(&self, key: &[u8], limit: usize) -> Result<Vec<Entry>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        if !self.runs {
            return Ok(self.arena.scan(self.roots.first().copied(), key, limit)?);
        }
        // Each run has its own seek cursor. Empty values are tombstones; the
        // newest run wins before limit and ordering are applied.
        let batch = limit.clamp(1, 64);
        let mut cursors: Vec<_> = self
            .roots
            .iter()
            .map(|&id| RunCursor::open(&self.arena, id, key, batch))
            .collect::<io::Result<_>>()?;
        let mut out = Vec::new();
        while out.len() < limit {
            let Some(entry) = RunCursor::next_visible(&mut cursors, &self.arena, batch)? else {
                break;
            };
            out.push(entry);
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
        self.arena = Self::create_arena(&self.path, self.epoch)?;
        File::open(&self.path)?.sync_all()?;
        self.bulk(rows)?;
        fs::remove_file(self.path.join(format!("arena-{old}")))?;
        File::open(&self.path)?.sync_all()?;
        Ok(())
    }
}
// The ordered-map oracle models a change independently of each storage backend.
pub fn apply(rows: &mut Rows, edits: &Edits) {
    for (key, _) in edits.iter().filter(|(_, value)| value.is_none()) {
        rows.remove(key);
    }
    rows.extend(
        edits
            .iter()
            .filter_map(|(key, value)| value.as_ref().map(|value| (key.clone(), value.clone()))),
    );
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

fn edit_sql_row(
    insert: &mut rusqlite::Statement<'_>,
    delete: &mut rusqlite::Statement<'_>,
    key: &[u8],
    value: Option<&[u8]>,
) -> Result<()> {
    match value {
        Some(value) => insert.execute(params![key, value])?,
        None => delete.execute([key])?,
    };
    Ok(())
}

impl Sql {
    fn bulk(&mut self, rows: Rows) -> Result<()> {
        match self.layout {
            SqlLayout::Rows => self.bulk_rows(rows),
            SqlLayout::BatchedRows => insert_batches(&mut self.conn, rows),
            SqlLayout::Blocks => crate::sql_blocks::bulk(&mut self.conn, rows),
        }
    }

    fn edit(&mut self, edits: &Edits) -> Result<()> {
        match self.layout {
            SqlLayout::Blocks => crate::sql_blocks::edit(&mut self.conn, edits),
            SqlLayout::Rows | SqlLayout::BatchedRows => self.edit_rows(edits),
        }
    }

    fn scan(&self, key: &[u8], limit: usize) -> Result<Vec<Entry>> {
        if self.layout == SqlLayout::Blocks {
            return crate::sql_blocks::scan(&self.conn, key, limit);
        }
        let mut q = self
            .conn
            .prepare_cached("SELECT k,v FROM rows WHERE k>=?1 ORDER BY k LIMIT ?2")?;
        let rows = q.query_map(
            params![key, i64::try_from(limit).unwrap_or(i64::MAX)],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn bulk_rows(&mut self, rows: Rows) -> Result<()> {
        let t = self.conn.transaction()?;
        {
            let mut q = t.prepare("INSERT INTO rows VALUES(?1,?2)")?;
            for (k, v) in rows {
                q.execute(params![k, v])?;
            }
        }
        t.commit()?;
        Ok(())
    }

    fn edit_rows(&mut self, edits: &Edits) -> Result<()> {
        let t = self.conn.transaction()?;
        {
            let mut ins = t.prepare("INSERT OR REPLACE INTO rows VALUES(?1,?2)")?;
            let mut del = t.prepare("DELETE FROM rows WHERE k=?1")?;
            for (key, value) in edits {
                edit_sql_row(&mut ins, &mut del, key, value.as_deref())?;
            }
        }
        t.commit()?;
        Ok(())
    }

    fn create(path: &Path, mode: &str) -> Result<Self> {
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
        Ok(Self { conn, path, layout })
    }
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
            "sqlite" | "sqlite-blocks" | "sqlite-batch" => Engine::Sql(Sql::create(path, mode)?),
            _ => return Err("unknown backend".into()),
        }))
    }
    pub fn bulk(&mut self, rows: Rows) -> Result<()> {
        match &mut self.0 {
            Engine::Packed(p) => p.bulk(rows),
            Engine::Sql(s) => s.bulk(rows),
        }
    }
    pub fn edit(&mut self, edits: &Edits) -> Result<()> {
        match &mut self.0 {
            Engine::Packed(p) => p.edit(edits),
            Engine::Sql(s) => s.edit(edits),
        }
    }
    pub fn scan(&self, key: &[u8], limit: usize) -> Result<Vec<Entry>> {
        match &self.0 {
            Engine::Packed(p) => p.scan(key, limit),
            Engine::Sql(s) => s.scan(key, limit),
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
    fn run_scan_refills_past_tombstones_and_keeps_the_newest_value() -> Result<()> {
        let path = std::env::temp_dir().join(format!("cx-run-scan-{}", std::process::id()));
        fs::create_dir(&path)?;
        let mut store = Pages::create(&path, true)?;
        let key = |n: u16| n.to_be_bytes().to_vec();
        let original: Rows = (0..150).map(|n| (key(n), vec![1])).collect();
        store.bulk(original)?;
        let mut edits: Edits = (0..130).map(|n| (key(n), None)).collect();
        edits.insert(key(130), Some(vec![2]));
        store.edit(&edits)?;
        store.edit(&Edits::from([(key(130), Some(vec![]))]))?;
        store.reopen()?;
        assert_eq!(
            store.scan(b"", 2)?,
            [(key(130), vec![]), (key(131), vec![1])]
        );
        assert!(store.scan(b"", 0)?.is_empty());
        fs::remove_dir_all(path)?;
        Ok(())
    }

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

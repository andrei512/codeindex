//! SQLite seeks by each block's largest key. A single ascending scan locates
//! the first eligible block and streams the rest until the caller has enough rows.
use crate::{
    store::Result,
    tree::{Edits, Entry, Rows},
};
use rusqlite::{Connection, OptionalExtension, Statement, params};

const BLOCK_BYTES: usize = 3584;

fn shared(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

fn put_length(bytes: &mut Vec<u8>, n: usize) -> Result<()> {
    if n < 255 {
        bytes.push(n as u8);
    } else {
        bytes.push(255);
        bytes.extend_from_slice(&u32::try_from(n)?.to_le_bytes());
    }
    Ok(())
}
fn length(bytes: &mut &[u8]) -> Result<usize> {
    let (n, rest) = bytes.split_first().ok_or("missing block length")?;
    *bytes = rest;
    if *n != 255 {
        return Ok(usize::from(*n));
    }
    let (n, rest) = bytes.split_at_checked(4).ok_or("truncated block length")?;
    *bytes = rest;
    Ok(u32::from_le_bytes(n.try_into()?) as usize)
}

fn append_row(block: &mut Vec<u8>, key: &[u8], value: &[u8], prefix: usize) -> Result<()> {
    for n in [prefix, key.len() - prefix, value.len()] {
        put_length(block, n)?;
    }
    block.extend_from_slice(&key[prefix..]);
    block.extend_from_slice(value);
    Ok(())
}

fn write_blocks(statement: &mut Statement<'_>, rows: Rows) -> Result<()> {
    let mut previous = Vec::new();
    let mut block = Vec::with_capacity(BLOCK_BYTES);
    for (key, value) in rows {
        let mut prefix = shared(&previous, &key);
        // At most 15 length bytes. A single oversize row may span SQLite pages.
        if !block.is_empty() && block.len() + 15 + key.len() - prefix + value.len() > BLOCK_BYTES {
            statement.execute(params![previous, block])?;
            block.clear();
            prefix = 0;
        }
        append_row(&mut block, &key, &value, prefix)?;
        previous = key;
    }
    if !block.is_empty() {
        statement.execute(params![previous, block])?;
    }
    Ok(())
}

fn decode_row<'a>(key: &mut Vec<u8>, data: &mut &'a [u8]) -> Result<&'a [u8]> {
    let prefix = length(data)?;
    let suffix_len = length(data)?;
    let value_len = length(data)?;
    if prefix > key.len() {
        return Err("invalid key prefix".into());
    }
    let (suffix, rest) = data.split_at_checked(suffix_len).ok_or("truncated key")?;
    let (value, next) = rest.split_at_checked(value_len).ok_or("truncated value")?;
    key.truncate(prefix);
    key.extend_from_slice(suffix);
    *data = next;
    Ok(value)
}

fn visit(mut data: &[u8], mut f: impl FnMut(&[u8], &[u8]) -> bool) -> Result<()> {
    let mut key = Vec::new();
    while !data.is_empty() {
        let value = decode_row(&mut key, &mut data)?;
        if !f(&key, value) {
            return Ok(());
        }
    }
    Ok(())
}

pub fn bulk(conn: &mut Connection, rows: Rows) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute_batch("CREATE TABLE rows(k BLOB PRIMARY KEY,v BLOB NOT NULL) WITHOUT ROWID")?;
    write_blocks(&mut tx.prepare("INSERT INTO rows VALUES(?1,?2)")?, rows)?;
    tx.commit()?;
    Ok(())
}

pub fn edit(conn: &mut Connection, edits: &Edits) -> Result<()> {
    let tx = conn.transaction()?;
    edit_blocks(&tx, edits)?;
    tx.commit()?;
    Ok(())
}

fn edit_blocks(conn: &Connection, edits: &Edits) -> Result<()> {
    let mut lookup = conn.prepare("SELECT k,v FROM rows WHERE k>=?1 ORDER BY k LIMIT 1")?;
    let mut del = conn.prepare("DELETE FROM rows WHERE k=?1")?;
    let mut ins = conn.prepare("INSERT INTO rows VALUES(?1,?2)")?;
    let mut pending = edits.iter().peekable();
    while let Some(&(key, _)) = pending.peek() {
        let block: Option<Entry> = lookup
            .query_row([key], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()?;
        let (mut items, upper) = take_block(&mut del, block)?;
        let group = std::iter::from_fn(|| {
            pending.next_if(|(key, _)| upper.as_ref().is_none_or(|end| *key <= end))
        });
        for (key, value) in group {
            patch_block_row(&mut items, key, value);
        }
        write_blocks(&mut ins, items)?;
    }
    Ok(())
}

fn patch_block_row(rows: &mut Rows, key: &Vec<u8>, value: &Option<Vec<u8>>) {
    match value {
        Some(value) => {
            rows.insert(key.clone(), value.clone());
        }
        None => {
            rows.remove(key);
        }
    }
}

fn take_block(delete: &mut Statement<'_>, block: Option<Entry>) -> Result<(Rows, Option<Vec<u8>>)> {
    let mut items = Rows::new();
    let Some((last, block)) = block else {
        return Ok((items, None));
    };
    visit(&block, |key, value| {
        items.insert(key.into(), value.into());
        true
    })?;
    delete.execute([&last])?;
    Ok((items, Some(last)))
}

pub fn scan(conn: &Connection, start: &[u8], limit: usize) -> Result<Vec<Entry>> {
    let mut result = Vec::new();
    if limit == 0 {
        return Ok(result);
    }
    let mut query = conn.prepare_cached("SELECT v FROM rows WHERE k>=?1 ORDER BY k")?;
    let mut blocks = query.query([start])?;
    while result.len() < limit {
        let Some(row) = blocks.next()? else {
            break;
        };
        scan_block(&mut result, row.get_ref(0)?.as_blob()?, start, limit)?;
    }
    Ok(result)
}

fn scan_block(result: &mut Vec<Entry>, block: &[u8], start: &[u8], limit: usize) -> Result<()> {
    visit(block, |key, value| {
        if key >= start {
            result.push((key.into(), value.into()));
        }
        result.len() < limit
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_delete_and_regrow_blocks_from_empty() -> Result<()> {
        let mut conn = Connection::open_in_memory()?;
        let mut expected = Rows::new();
        bulk(&mut conn, expected.clone())?;
        let make_rows = |offset: u32| -> Edits {
            (0..350u32)
                .map(|i| {
                    let mut k = vec![0; 280];
                    k.extend_from_slice(&(i * 2 + offset).to_be_bytes());
                    let size = if i == 99 { 32768 } else { (i % 180) as usize };
                    (k, Some(vec![i as u8; size]))
                })
                .collect()
        };
        let mut changes = make_rows(0);
        changes.insert(vec![], Some(vec![]));
        changes.insert(vec![255], Some(vec![255]));
        for update in [changes, make_rows(1)] {
            edit(&mut conn, &update)?;
            crate::store::apply(&mut expected, &update);
            for start in expected.keys().step_by(3) {
                for limit in [0, 1, 20, 99] {
                    let want: Vec<_> = expected
                        .range(start.clone()..)
                        .take(limit)
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect();
                    assert_eq!(scan(&conn, start, limit)?, want);
                }
            }
        }
        let drop_all: Edits = expected.keys().cloned().map(|k| (k, None)).collect();
        edit(&mut conn, &drop_all)?;
        assert!(scan(&conn, b"", 100)?.is_empty());
        edit(&mut conn, &Edits::from([(b"k".to_vec(), Some(Vec::new()))]))?;
        assert_eq!(
            scan(&conn, b"", usize::MAX)?,
            vec![(b"k".to_vec(), Vec::new())]
        );
        Ok(())
    }

    #[test]
    fn failed_batch_rolls_back_every_block() -> Result<()> {
        let mut conn = Connection::open_in_memory()?;
        let before: Rows = (0..200u32)
            .map(|i| (i.to_be_bytes().to_vec(), vec![9; 200]))
            .collect();
        bulk(&mut conn, before.clone())?;
        conn.execute_batch("CREATE TRIGGER reject_last BEFORE INSERT ON rows WHEN NEW.k=x'FFFFFFFF' BEGIN SELECT RAISE(ABORT, 'injected failure'); END;")?;
        let edits = Edits::from([
            (1u32.to_be_bytes().into(), None),
            (u32::MAX.to_be_bytes().into(), Some(vec![1])),
        ]);
        assert!(edit(&mut conn, &edits).is_err());
        assert!(scan(&conn, b"", usize::MAX)?.into_iter().eq(before));
        Ok(())
    }
}

mod sql_blocks;
mod store;
mod tree;
mod workload;
use std::{collections::BTreeSet, env, fs, path::Path, time::Instant};
use store::{Result, Store, apply};
use tree::{Entry, Rows};
fn micros(time: Instant) -> u128 {
    time.elapsed().as_micros()
}
fn bytes(path: &Path) -> Result<u64> {
    let mut total = 0;
    for e in fs::read_dir(path)? {
        total += e?.metadata()?.len();
    }
    Ok(total)
}
fn expected(rows: &Rows, from: &Vec<u8>, limit: usize) -> Vec<Entry> {
    rows.range(from.clone()..)
        .take(limit)
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}
fn run() -> Result<()> {
    let args: Vec<_> = env::args().collect();
    if args.len() != 4 {
        return Err(
            "usage: cx-storage-replay REPO NEW_STORE cow|runs|sqlite|sqlite-batch|sqlite-blocks"
                .into(),
        );
    }
    let path = Path::new(&args[2]);
    let trace = workload::trace(Path::new(&args[1]))?;
    let mut truth = trace.initial.clone();
    let now = Instant::now();
    let mut s = Store::create(path, &args[3])?;
    s.bulk(trace.initial)?;
    s.reopen()?;
    let fresh = micros(now);
    println!(
        "fresh_us {fresh}\nrows {}\nchanged {}\nbuild_bytes {}",
        truth.len(),
        trace.edits.len(),
        bytes(path)?
    );
    let mut keys: BTreeSet<_> = truth
        .keys()
        .step_by((truth.len() / 64).max(1))
        .cloned()
        .collect();
    keys.insert(vec![255]);
    keys.extend(trace.edits.keys().cloned());
    for i in 0..32 {
        let edits = if i % 2 == 0 {
            &trace.edits
        } else {
            &trace.undo
        };
        let t = Instant::now();
        s.edit(edits)?;
        println!("edit_us {}", micros(t));
        apply(&mut truth, edits);
        let t = Instant::now();
        s.reopen()?;
        println!("reopen_us {}", micros(t));
        for k in &keys {
            let want = expected(&truth, k, 20);
            let t = Instant::now();
            let answer = s.scan(k, 20)?;
            let elapsed = micros(t);
            if answer != want {
                return Err("range scan differs from fresh projection".into());
            }
            println!("query_us {elapsed}");
        }
    }
    println!("churn_bytes {}", bytes(path)?);
    let t = Instant::now();
    s.compact()?;
    s.reopen()?;
    println!("maintenance_us {}", micros(t));
    assert!(
        s.scan(b"", usize::MAX)?
            .iter()
            .map(|(k, v)| (k, v))
            .eq(truth.iter())
    );
    println!("final_bytes {}", bytes(path)?);
    // Query sampling covers every timed generation. Also compare the complete
    // changed-state projection once, outside all timing and size measurements.
    s.edit(&trace.edits)?;
    s.reopen()?;
    apply(&mut truth, &trace.edits);
    assert!(s.scan(b"", usize::MAX)?.into_iter().eq(truth));
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::{Arena, Edits};
    use std::fs::OpenOptions;
    #[test]
    fn old_cursor_crosses_a_split_and_deleted_leaves() {
        let path = env::temp_dir().join(format!("cx-arena-test-{}", std::process::id()));
        let file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .append(true)
            .open(&path)
            .unwrap();
        fs::remove_file(path).unwrap();
        let mut a = Arena::new(file).unwrap();
        let mut truth: Rows = (0..5000u64)
            .map(|i| ((i * 2).to_be_bytes().to_vec(), vec![17; 24]))
            .collect();
        let old = a.bulk(truth.clone()).unwrap();
        let before = truth.clone();
        let edits: Edits = (0..5000u64)
            .map(|i| (i.to_be_bytes().to_vec(), (i % 3 == 0).then(|| vec![42; 60])))
            .collect();
        let new = a.update(old, &edits).unwrap();
        apply(&mut truth, &edits);
        for (root, map) in [(old, before), (new, truth)] {
            assert!(a.scan(root, b"", usize::MAX).unwrap().into_iter().eq(map));
        }
    }
}

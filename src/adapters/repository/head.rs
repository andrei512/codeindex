//! Read checkout provenance directly from the files-backed Git ref store.
use super::{git_dirs,invalid,optional_bytes};
use std::{io,path::{Component,Path}};
fn oid(s:&str)->bool {matches!(s.len(),40|64)&&s.bytes().all(|b|b.is_ascii_hexdigit())}
pub fn head_commit(root:&Path)->io::Result<Option<String>> {
    let (git,common)=git_dirs(root)?;
    let Some(head)=optional_bytes(&git.join("HEAD"))? else{return Ok(None)};
    let mut value=String::from_utf8(head).map_err(invalid)?;
    for _ in 0..16 {
        let s=value.trim_end();
        if oid(s){return Ok(Some(s.to_ascii_lowercase()));}
        let name=s.strip_prefix("ref: ").filter(|s| s.starts_with("refs/") && Path::new(s).components().all(|p|matches!(p,Component::Normal(_)))).ok_or_else(||invalid("invalid Git HEAD reference"))?;
        let dir=if name.starts_with("refs/worktree/")||name.starts_with("refs/bisect/")||name.starts_with("refs/rewritten/"){&git}else{&common};
        if let Some(bytes)=optional_bytes(&dir.join(name))?{value=String::from_utf8(bytes).map_err(invalid)?;continue;}
        if let Some(bytes)=optional_bytes(&common.join("packed-refs"))?{
            for line in bytes.split(|&b|b==b'\n'){
                if let Some(i)=line.iter().position(|&b|b==b' ') && &line[i+1..]==name.as_bytes(){
                    let id=std::str::from_utf8(&line[..i]).map_err(invalid)?;
                    if !oid(id){return Err(invalid("invalid packed Git object ID"));}
                    return Ok(Some(id.to_ascii_lowercase()));
                }
            }
        }
        return Ok(None)
    }
    Err(invalid("cyclic Git HEAD reference"))
}

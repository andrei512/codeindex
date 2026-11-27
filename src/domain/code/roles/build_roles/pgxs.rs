//! Match an existing SQL script against PostgreSQL extension regression targets.
use super::*;
use crate::domain::code::make_reference::Delimiter;
use std::borrow::Cow;

// Evaluate only expressions that can select this existing SQL file.
struct Expansion<'a> {
    vars: &'a HashMap<String, String>,
    file: &'a str,
    remaining: usize,
}

impl Expansion<'_> {
    fn value<'s>(&mut self, value: &'s str) -> Cow<'s, str> {
        if self.remaining == 0 {
            return Cow::Borrowed("");
        }
        self.remaining -= 1;
        if !value.contains("$(") {
            return Cow::Borrowed(value);
        }
        self.concatenate(value)
            .map_or(Cow::Borrowed(""), Cow::Owned)
    }

    fn concatenate(&mut self, mut tail: &str) -> Option<String> {
        let mut result = String::new();
        while let Some((prefix, rest)) = tail.split_once("$(") {
            let (expr, remainder) = Delimiter::Paren.payload(rest)?;
            result.push_str(prefix);
            self.append_piece(expr, &mut result)?;
            tail = remainder;
        }
        result.push_str(tail);
        Some(result)
    }

    fn append_piece(&mut self, expr: &str, result: &mut String) -> Option<()> {
        (self.remaining != 0 && self.append(expr, result)).then_some(())
    }

    fn append(&mut self, expr: &str, result: &mut String) -> bool {
        if let Some(args) = expr.strip_prefix("wildcard ") {
            self.wildcard(args, result);
        } else if let Some(args) = expr.strip_prefix("patsubst ") {
            return self.patsubst(args, result);
        } else if let Some(value) = self.vars.get(expr) {
            result.push_str(&self.value(value));
        }
        true
    }

    fn wildcard(&mut self, args: &str, result: &mut String) {
        if self
            .value(args)
            .split_whitespace()
            .any(|pat| matches_wildcard(pat, self.file))
        {
            result.push_str(self.file);
        }
    }

    fn patsubst(&mut self, args: &str, result: &mut String) -> bool {
        let mut parts = args.splitn(3, ',');
        let (Some(pattern), Some(replacement), Some(list)) =
            (parts.next(), parts.next(), parts.next())
        else {
            return false;
        };
        substitute_words(pattern, replacement, &self.value(list), result);
        true
    }
}

fn matches_wildcard(pattern: &str, file: &str) -> bool {
    pattern == file
        || pattern.split_once('*').is_some_and(|(prefix, suffix)| {
            file.strip_prefix(prefix)
                .and_then(|v| v.strip_suffix(suffix))
                .is_some_and(|middle| !middle.contains('/'))
        })
}

fn substitute_words(pattern: &str, replacement: &str, list: &str, out: &mut String) {
    let pattern = pattern.split_once('%');
    for (index, word) in list.split_whitespace().enumerate() {
        if index != 0 {
            out.push(' ');
        }
        let stem =
            pattern.and_then(|(prefix, suffix)| word.strip_prefix(prefix)?.strip_suffix(suffix));
        substitute_word(word, stem, replacement, out);
    }
}

fn substitute_word(word: &str, stem: Option<&str>, replacement: &str, out: &mut String) {
    let Some(stem) = stem else {
        out.push_str(word);
        return;
    };
    let mut parts = replacement.split('%');
    out.push_str(parts.next().unwrap_or(""));
    for part in parts {
        out.push_str(stem);
        out.push_str(part);
    }
}

fn make_value<'s>(value: &'s str, vars: &HashMap<String, String>, file: &str) -> Cow<'s, str> {
    let mut expansion = Expansion {
        vars,
        file,
        remaining: 128,
    };
    let result = expansion.value(value);
    if expansion.remaining == 0 {
        Cow::Borrowed("")
    } else {
        result
    }
}
fn assign(vars: &mut HashMap<String, String>, line: &str) {
    let line = line.split('#').next().unwrap_or("");
    let Some((lhs, rhs)) = line.split_once('=') else {
        return;
    };
    let lhs = lhs.trim();
    let name = lhs.trim_end_matches([':', '+', '?']).trim();
    if name.is_empty() || name.contains(char::is_whitespace) {
        return;
    }
    if lhs.ends_with('+') {
        let v = vars.entry(name.to_owned()).or_default();
        v.push(' ');
        v.push_str(rhs.trim())
    } else if !lhs.ends_with('?') || !vars.contains_key(name) {
        vars.insert(name.to_owned(), rhs.trim().to_owned());
    }
}

fn includes_pgxs(line: &str) -> bool {
    line.trim().strip_prefix("include ").is_some_and(|paths| {
        paths
            .split_whitespace()
            .any(|p| p == "$(PGXS)" || p.ends_with("/pgxs.mk"))
    })
}

fn regress_variables(make: &str) -> Option<HashMap<String, String>> {
    if !make.contains("$(PGXS)") && !make.contains("/pgxs.mk") {
        return None;
    }
    let mut uses_pgxs = false;
    let mut vars = HashMap::new();
    for line in make.lines() {
        uses_pgxs |= includes_pgxs(line);
        if !line.starts_with('\t') {
            assign(&mut vars, line);
        }
    }
    uses_pgxs.then_some(vars)
}

// pg_regress accepts both forms and the last supplied directory wins.
fn input_directory(opts: &str) -> &str {
    let mut words = opts.split_whitespace();
    let dirs = std::iter::from_fn(|| {
        let option = words.next()?;
        Some(match option {
            "--inputdir" => Some(words.next().unwrap_or(".")),
            _ => option.strip_prefix("--inputdir="),
        })
    });
    dirs.flatten().last().unwrap_or(".")
}

#[derive(Default)]
pub(super) struct Pgxs {
    manifests: HashMap<PathBuf, Option<HashMap<String, String>>>,
}

impl Pgxs {
    pub(super) fn selects(
        &mut self,
        makefile: PathBuf,
        file: &str,
        read: &mut Reader<'_>,
    ) -> io::Result<bool> {
        use std::collections::hash_map::Entry;
        let vars = match self.manifests.entry(makefile) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                let vars = read(entry.key())?.as_deref().and_then(parse_manifest);
                entry.insert(vars)
            }
        };
        Ok(vars.as_ref().is_some_and(|vars| selected(vars, file)))
    }
}

fn parse_manifest(make: &str) -> Option<HashMap<String, String>> {
    let joined;
    let make = if make.contains("\\\n") {
        joined = make.replace("\\\n", " ");
        joined.as_str()
    } else {
        make
    };
    regress_variables(make)
}

#[cfg(test)]
pub(super) fn pg_regress(make: &str, file: &str) -> bool {
    parse_manifest(make).is_some_and(|vars| selected(&vars, file))
}

fn selected(vars: &HashMap<String, String>, file: &str) -> bool {
    let Some(regress) = vars.get("REGRESS") else {
        return false;
    };
    let opts = make_value(
        vars.get("REGRESS_OPTS").map_or("", |v| v.as_ref()),
        vars,
        file,
    );
    let dir = input_directory(&opts);
    let Some(folder) = script_path(Path::new(""), dir).map(|d| d.join("sql")) else {
        return false;
    };
    let requested = Path::new(file);
    let local_case = requested
        .parent()
        .filter(|parent| *parent == folder)
        .and_then(|_| requested.file_name()?.to_str()?.strip_suffix(".sql"));
    make_value(regress, vars, file)
        .split_whitespace()
        .any(|case| {
            if case.contains(std::path::is_separator) || cfg!(windows) && case.contains(':') {
                folder.join(format!("{case}.sql")) == requested
            } else {
                Some(case) == local_case
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_manifest_is_read_once_and_failed_reads_can_retry() {
        let mut context = Pgxs::default();
        let path = PathBuf::from("Makefile");
        assert!(
            context
                .selects(path.clone(), "sql/a.sql", &mut |_| Err(io::Error::other(
                    "read failed"
                )))
                .is_err()
        );
        let mut reads = 0;
        for name in ["a", "b", "other"] {
            let found = context
                .selects(path.clone(), &format!("sql/{name}.sql"), &mut |_| {
                    reads += 1;
                    Ok(Some("include $(PGXS)\nREGRESS = a b".into()))
                })
                .unwrap();
            assert_eq!(found, name != "other");
        }
        assert_eq!(reads, 1);
        assert!(
            !Pgxs::default()
                .selects(path, "sql/a.sql", &mut |_| Ok(None))
                .unwrap()
        );
    }

    #[test]
    fn substitution_preserves_word_boundaries_and_expands_each_percent() {
        let vars = HashMap::from([("CASES".to_owned(), "x/α.sql y x/β.sql".to_owned())]);
        assert_eq!(
            make_value("a$(patsubst x/%.sql,%:%,$(CASES))b", &vars, ""),
            "aα:α y β:βb"
        );
        assert_eq!(
            make_value("a$(patsubst x/%.sql,,$(CASES))b", &vars, ""),
            "a y b"
        );
    }

    #[test]
    fn malformed_nested_value_discards_its_own_prefix() {
        let vars = HashMap::from([("BAD".to_owned(), "partial$(patsubst invalid)".to_owned())]);
        assert_eq!(make_value("before$(BAD)after", &vars, ""), "beforeafter");
    }

    #[test]
    fn regression_membership_accepts_relative_components() {
        let make = "include $(PGXS)\nREGRESS = ./nested/test ./simple last";
        assert!(pg_regress(make, "sql/nested/test.sql"));
        assert!(pg_regress(make, "sql/simple.sql"));
        assert!(!pg_regress(make, "nested/test.sql"));
    }
}

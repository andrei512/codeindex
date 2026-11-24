use super::{HashMap, HashSet, Language, ROOT, Syntax};

pub(super) struct ShadowBindings<'a> {
    locals: HashMap<(usize, &'a str), usize>,
    parameters: HashSet<(usize, &'a str)>,
}

impl ShadowBindings<'_> {
    pub(super) fn contains(
        &self,
        parents: &[usize],
        use_at: usize,
        name: &str,
        caller: Option<usize>,
    ) -> bool {
        if caller.is_some_and(|id| self.parameters.contains(&(id, name))) {
            return true;
        }
        let mut scope = parents[use_at];
        while scope != ROOT {
            if self
                .locals
                .get(&(scope, name))
                .is_some_and(|&start| start <= use_at)
            {
                return true;
            }
            scope = parents[scope];
        }
        false
    }
}

impl<'a> Syntax<'a> {
    pub(super) fn shadow_bindings(
        &self,
        owner: &[Option<usize>],
        definitions: &HashSet<usize>,
        signature_tokens: &mut [bool],
    ) -> ShadowBindings<'a> {
        let mut locals = HashMap::new();
        let mut parameters = HashSet::new();
        for (i, t) in self.t.iter().enumerate() {
            if self.parent[i] == ROOT {
                continue;
            }
            // A short declaration can introduce several Go locals at once.
            if self.lang == Language::Go && t.text == ":" && self.w(i + 1) == "=" {
                let mut j = i;
                while j > 0 && self.id(j - 1) {
                    locals.entry((self.parent[i], self.w(j - 1))).or_insert(i);
                    if j < 2 || self.w(j - 2) != "," {
                        break;
                    }
                    j -= 2;
                }
            }
            if self.id(i)
                && (definitions.contains(&i) || self.local_binding(i))
                && (owner[i].is_some() || self.kotlin_initializer_type(i).is_some())
            {
                locals.entry((self.parent[i], t.text)).or_insert(i);
            }
            // Anonymous callbacks have no indexed caller declaration. Their
            // parameters still hide outer callables inside the callback body.
            if t.text == "("
                && let Some(end) = self.close(i)
                && self.w(end + 1) == "{"
                && owner[i].is_some()
                && match self.lang {
                    Language::Go => self.prev(i) == "func",
                    Language::Dart => {
                        !matches!(self.prev(i), "if" | "while" | "for" | "switch" | "catch")
                    }
                    Language::ObjectiveC => self.prev(i) == "^",
                    _ => false,
                }
            {
                for j in i + 1..end {
                    if self.id(j) {
                        locals.entry((end + 1, self.w(j))).or_insert(j);
                        signature_tokens[j] = true;
                    }
                }
            }
            if self.lang == Language::Kotlin && t.text == "-" && self.w(i + 1) == ">" {
                // Only the leading parameters of a lambda introduce names. A
                // when branch may also use an arrow, after its condition.
                let open = self.parent[i];
                if (open + 1..i).all(|j| self.id(j) || matches!(self.w(j), "," | ":")) {
                    for j in open + 1..i {
                        if self.id(j) {
                            locals.entry((open, self.w(j))).or_insert(j);
                            signature_tokens[j] = true;
                        }
                    }
                }
            }
        }
        for (id, d) in self.decls.iter().enumerate() {
            if let Some(r) = &d.signature {
                for i in r.clone() {
                    if self.id(i) && i != d.at {
                        parameters.insert((id, self.w(i)));
                    }
                }
            }
        }
        ShadowBindings { locals, parameters }
    }
    // Keep binding occurrences separate from reads of a callable as a value.
    fn local_binding(&self, i: usize) -> bool {
        let next = self.w(i + 1);
        let prev = self.prev(i);
        match self.lang {
            Language::Go => matches!(prev, "var" | "const"),
            Language::Dart | Language::ObjectiveC => {
                (i > 0 && self.id(i - 1) && !matches!(prev, "return" | "throw" | "case")
                    || matches!(prev, "*" | ">" | "?"))
                    && matches!(next, "=" | ";" | "," | ")")
            }
            _ => false,
        }
    }
}

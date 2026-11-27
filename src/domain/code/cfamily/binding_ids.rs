//! Most names have a single declaration. Overloads retain declaration order.
enum BindingIds {
    One(usize),
    Many(Vec<usize>),
}

impl BindingIds {
    pub fn as_slice(&self) -> &[usize] {
        match self {
            Self::One(id) => std::slice::from_ref(id),
            Self::Many(ids) => ids,
        }
    }

    pub fn push(&mut self, id: usize) {
        match self {
            Self::One(first) => *self = Self::Many(vec![*first, id]),
            Self::Many(ids) => ids.push(id),
        }
    }
}

// Empty blocks and blocks with one name need no hash table. Only promoted
// scopes occupy an entry in the parser's table arena.
#[derive(Default)]
pub(super) enum Names<'a> {
    #[default]
    Empty,
    One(&'a str, usize),
    Table(usize),
}

type Table<'a> = std::collections::HashMap<&'a str, BindingIds>;

#[derive(Default)]
pub(super) struct Tables<'a>(Vec<Table<'a>>);

impl<'a> Tables<'a> {
    pub fn ids<'s>(&'s self, names: &'s Names<'a>, name: &str) -> &'s [usize] {
        match names {
            Names::Empty => &[],
            Names::One(key, id) if *key == name => std::slice::from_ref(id),
            Names::One(..) => &[],
            Names::Table(index) => self.0[*index].get(name).map_or(&[], BindingIds::as_slice),
        }
    }

    pub fn add(&mut self, names: &mut Names<'a>, key: &'a str, id: usize) {
        let index = match *names {
            Names::Empty => {
                *names = Names::One(key, id);
                return;
            }
            Names::One(prev, first) => {
                let index = self.0.len();
                self.0.push(Table::from([(prev, BindingIds::One(first))]));
                *names = Names::Table(index);
                index
            }
            Names::Table(index) => index,
        };
        self.0[index]
            .entry(key)
            .and_modify(|ids| ids.push(id))
            .or_insert(BindingIds::One(id));
    }
}

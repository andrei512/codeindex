//! Module edges and Cargo externs bound the files a Rust lookup or impl can
//! reach. Include inactive module edges because they affect root discovery.
pub(in crate::domain::code::rust) struct Components(Vec<usize>);

impl Components {
    pub fn new(n: usize) -> Self {
        Self((0..n).collect())
    }
    fn root(&mut self, mut id: usize) -> usize {
        while self.0[id] != id {
            self.0[id] = self.0[self.0[id]];
            id = self.0[id];
        }
        id
    }
    pub fn join(&mut self, a: usize, b: usize) {
        let a = self.root(a);
        let b = self.root(b);
        self.0[a] = b;
    }
    pub fn finish(mut self) -> Vec<usize> {
        for id in 0..self.0.len() {
            self.0[id] = self.root(id)
        }
        self.0
    }
}

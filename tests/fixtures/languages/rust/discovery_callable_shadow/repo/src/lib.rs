fn marker() {}
fn shadow(marker: fn()) { marker(); }
pub fn positive() { marker(); }

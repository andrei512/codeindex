//! Versioned by the extraction fingerprint in the generation manifest.
//! Collection order is canonical so repeated publications have identical bytes.
mod scalar;
use std::{collections::{HashMap, HashSet}, hash::Hash, io};
pub(super) fn bad()->io::Error{io::Error::new(io::ErrorKind::InvalidData,"invalid cached parser evidence")}
pub(super) trait Output {
    fn overflowed(&self) -> bool { false }
    fn push(&mut self, byte: u8);
    fn extend_from_slice(&mut self, bytes: &[u8]);
}
impl Output for Vec<u8> {
    fn push(&mut self, byte: u8) { Vec::push(self, byte); }
    fn extend_from_slice(&mut self, bytes: &[u8]) { Vec::extend_from_slice(self, bytes); }
}
/// Preallocated workspace. The serial writer resumes at the first incomplete row.
pub(crate) struct BoundedOutput {
    pub(crate) data: Vec<u8>,
    pub(crate) overflowed: bool,
    limit: usize,
}
impl BoundedOutput {
    pub(crate) fn new(limit: usize) -> Option<Self> {
        let mut data = Vec::new();
        data.try_reserve_exact(limit).ok()?;
        Some(Self { data, overflowed: false, limit })
    }
}
impl Output for BoundedOutput {
    fn overflowed(&self) -> bool { self.overflowed }
    #[inline]
    fn push(&mut self, byte: u8) {
        if self.data.len() < self.limit {
            self.data.push(byte);
        } else {
            self.overflowed = true;
        }
    }
    #[inline]
    fn extend_from_slice(&mut self, bytes: &[u8]) {
        if bytes.len() <= self.limit - self.data.len() {
            self.data.extend_from_slice(bytes);
        } else {
            self.overflowed = true;
        }
    }
}
pub(super) trait Wire:Sized{
 fn save(&self,out:&mut impl crate::domain::code::cache::Output);
 fn load(input:&mut &[u8])->io::Result<Self>;
}
fn read_items<T: Wire>(input: &mut &[u8]) -> io::Result<impl ExactSizeIterator<Item = io::Result<T>>> {
    let count = usize::load(input)?;
    if count > input.len() {
        return Err(bad());
    }
    Ok((0..count).map(|_| T::load(input)))
}

fn write_items<T, W: Output>(
    out: &mut W,
    items: impl IntoIterator<Item = T>,
    mut write: impl FnMut(T, &mut W),
) {
    for item in items {
        if out.overflowed() {
            break;
        }
        write(item, out);
    }
}

impl<T: Wire> Wire for Vec<T> {
    fn save(&self, out: &mut impl Output) {
        self.len().save(out);
        write_items(out, self, |item, out| item.save(out));
    }

    fn load(input: &mut &[u8]) -> io::Result<Self> {
        let items = read_items(input)?;
        let mut values = Vec::with_capacity(items.len());
        for item in items {
            values.push(item?);
        }
        Ok(values)
    }
}
impl<T:Wire> Wire for Option<T>{fn save(&self,w:&mut impl crate::domain::code::cache::Output){self.is_some().save(w);if let Some(v)=self{v.save(w)}}fn load(r:&mut &[u8])->io::Result<Self>{bool::load(r)?.then(||T::load(r)).transpose()}}
impl<T:Wire> Wire for Box<T>{fn save(&self,w:&mut impl crate::domain::code::cache::Output){self.as_ref().save(w)}fn load(r:&mut &[u8])->io::Result<Self>{Ok(Box::new(T::load(r)?))}}
// The length prefix can exhaust a bounded row before its entries need sorting.
fn write_ordered<T, K: Ord, W: Output>(
    out: &mut W,
    entries: impl ExactSizeIterator<Item = T>,
    key: impl FnMut(&T) -> K,
    write: impl FnMut(T, &mut W),
) {
    entries.len().save(out);
    if out.overflowed() {
        return;
    }
    let mut ordered: Vec<_> = entries.collect();
    ordered.sort_unstable_by_key(key);
    write_items(out, ordered, write);
}

impl<K: Wire + Hash + Ord, V: Wire> Wire for HashMap<K, V> {
    fn save(&self, out: &mut impl Output) {
        write_ordered(out, self.iter(), |&(key, _)| key, |(key, value), out| {
            key.save(out);
            if !out.overflowed() {
                value.save(out);
            }
        });
    }

    fn load(input: &mut &[u8]) -> io::Result<Self> {
        let mut map = Self::new();
        for entry in read_items::<(K, V)>(input)? {
            let (key, value) = entry?;
            map.insert(key, value);
        }
        Ok(map)
    }
}

impl<K: Wire + Hash + Ord> Wire for HashSet<K> {
    fn save(&self, out: &mut impl Output) {
        write_ordered(out, self.iter(), |&key| key, |item, out| item.save(out));
    }

    fn load(input: &mut &[u8]) -> io::Result<Self> {
        read_items(input)?.collect()
    }
}
macro_rules! tuple{($($t:ident:$i:tt),+) =>{impl<$($t:Wire),+> Wire for ($($t,)+){fn save(&self,w:&mut impl crate::domain::code::cache::Output){$(self.$i.save(w);)+}fn load(r:&mut &[u8])->io::Result<Self>{Ok(($($t::load(r)?,)+))}}}}
tuple!(A:0,B:1);tuple!(A:0,B:1,C:2);tuple!(A:0,B:1,C:2,D:3);
macro_rules! record {($t:ident {$($f:ident),*})=>{impl $crate::domain::code::cache::Wire for $t{
fn save(&self,w:&mut impl crate::domain::code::cache::Output){$(self.$f.save(w);)*}
fn load(r:&mut &[u8])->std::io::Result<Self>{use $crate::domain::code::cache::Wire;Ok(Self{$($f:Wire::load(r)?),*})}
}}}
pub(super) use record;

// Each tag and its payload order are specified once for both wire directions.
macro_rules! encode_tagged {
    ($value:ident, $out:ident; $($tag:literal => $variant:pat => [$($item:ident),*]),+) => {
        match $value {
            $($variant => {
                ($tag as usize).save($out);
                $($item.save($out);)*
            }),+
        }
    }
}
pub(super) use encode_tagged;

macro_rules! tagged {
    ($ty:ident { $($tag:literal => $variant:ident $(( $($item:ident),+ ))? $({$($field:ident),+})?),+ $(,)? }) => {
        impl $crate::domain::code::cache::Wire for $ty {
            fn save(&self, out: &mut impl $crate::domain::code::cache::Output) {
                $crate::domain::code::cache::encode_tagged!(self, out;
                    $($tag => Self::$variant $(($($item),+))? $({ $($field),+ })?
                        => [$($($item),+)? $($($field),+)?]),+
                );
            }

            fn load(input: &mut &[u8]) -> std::io::Result<Self> {
                use $crate::domain::code::cache::Wire;
                #[deny(unreachable_patterns)]
                match <usize as Wire>::load(input)? {
                    $($tag => {
                        $($(let $item = Wire::load(input)?;)+)?
                        Ok(Self::$variant $(($($item),+))? $({$($field: Wire::load(input)?),+})?)
                    }),+,
                    _ => Err($crate::domain::code::cache::bad()),
                }
            }
        }
    };
}
pub(super) use tagged;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exhausted_output_skips_map_and_set_sorts() {
        #[derive(PartialEq, Eq, Hash)]
        struct Key(usize);
        impl PartialOrd for Key {
            fn partial_cmp(&self, rhs: &Self) -> Option<std::cmp::Ordering> { Some(self.cmp(rhs)) }
        }
        impl Ord for Key {
            fn cmp(&self, _: &Self) -> std::cmp::Ordering {
                panic!("sorting a discarded row");
            }
        }
        impl Wire for Key {
            fn save(&self, out: &mut impl Output) { self.0.save(out); }
            fn load(input: &mut &[u8]) -> io::Result<Self> { usize::load(input).map(Key) }
        }
        let mut sink = BoundedOutput::new(0).unwrap();
        HashMap::from([(Key(1), true), (Key(2), false)]).save(&mut sink);
        HashSet::from([Key(1), Key(2)]).save(&mut sink);
        assert!(sink.overflowed);
        assert!(sink.data.is_empty());
    }

    #[test]
    fn an_overflowing_item_stops_the_rest_of_its_row() {
        struct Item(bool);
        impl Wire for Item {
            fn save(&self, out: &mut impl Output) {
                assert!(self.0, "attempted to encode a discarded suffix");
                out.extend_from_slice(b"payload");
            }
            fn load(_: &mut &[u8]) -> io::Result<Self> { unreachable!() }
        }
        let mut sink = BoundedOutput::new(3).unwrap();
        vec![Item(true), Item(false)].save(&mut sink);
        assert!(sink.overflowed());
    }

    #[test]
    fn ordered_collection_wire_preserves_written_order_and_admits_old_rows() {
        let values: HashMap<usize, usize> = HashMap::from([(2, 9), (1, 8)]);
        let mut bytes = Vec::new();
        values.save(&mut bytes);
        assert_eq!(bytes, [2, 1, 8, 2, 9]);
        let mut old = &[3, 2, 9, 1, 7, 1, 8][..];
        assert_eq!(HashMap::<usize, usize>::load(&mut old).unwrap(), values);
        assert!(old.is_empty());

        let values: HashSet<usize> = HashSet::from([2, 1]);
        bytes.clear();
        values.save(&mut bytes);
        assert_eq!(bytes, [2, 1, 2]);
        assert_eq!(
            HashSet::<usize>::load(&mut &[3, 2, 1, 1][..]).unwrap(),
            values
        );
    }

    #[test]
    fn collection_rows_reject_bad_prefixes_and_truncated_values() {
        let unterminated = [0xff; (usize::BITS as usize).div_ceil(7)];
        assert!(Vec::<usize>::load(&mut unterminated.as_slice()).is_err());
        assert!(HashSet::<usize>::load(&mut &[2, 1][..]).is_err());
        assert!(HashMap::<usize, usize>::load(&mut &[1, 2][..]).is_err());
    }
}

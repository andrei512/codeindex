//! Versioned by the extraction fingerprint in the generation manifest.
//! Collection order is canonical so repeated publications have identical bytes.
use std::{collections::{HashMap,HashSet},hash::Hash,io,ops::Range};
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
impl Wire for usize{
 fn save(&self,w:&mut impl crate::domain::code::cache::Output){let mut n=*self;while n>=128{w.push((n as u8)|128);n>>=7;}w.push(n as u8);}
 fn load(r:&mut &[u8])->io::Result<Self>{let mut n=0;for shift in (0..usize::BITS).step_by(7){let (&b,rest)=r.split_first().ok_or_else(bad)?;*r=rest;let part=usize::from(b&127);if part>usize::MAX>>shift{return Err(bad())}n|=part<<shift;if b<128{return Ok(n)}}Err(bad())}
}
impl Wire for bool{fn save(&self,w:&mut impl crate::domain::code::cache::Output){w.push(*self as u8)}fn load(r:&mut &[u8])->io::Result<Self>{match usize::load(r)?{0=>Ok(false),1=>Ok(true),_=>Err(bad())}}}
impl Wire for String{fn save(&self,w:&mut impl crate::domain::code::cache::Output){self.len().save(w);w.extend_from_slice(self.as_bytes())}fn load(r:&mut &[u8])->io::Result<Self>{let n=usize::load(r)?;let (v,rest)=r.split_at_checked(n).ok_or_else(bad)?;*r=rest;Ok(std::str::from_utf8(v).map_err(|_|bad())?.into())}}
impl<T:Wire> Wire for Vec<T>{fn save(&self,w:&mut impl crate::domain::code::cache::Output){self.len().save(w);for t in self{t.save(w)}}fn load(r:&mut &[u8])->io::Result<Self>{let n=usize::load(r)?;if n>r.len(){return Err(bad())}let mut v=Vec::new();for _ in 0..n{v.push(T::load(r)?)}Ok(v)}}
impl<T:Wire> Wire for Option<T>{fn save(&self,w:&mut impl crate::domain::code::cache::Output){self.is_some().save(w);if let Some(v)=self{v.save(w)}}fn load(r:&mut &[u8])->io::Result<Self>{bool::load(r)?.then(||T::load(r)).transpose()}}
impl<T:Wire> Wire for Box<T>{fn save(&self,w:&mut impl crate::domain::code::cache::Output){self.as_ref().save(w)}fn load(r:&mut &[u8])->io::Result<Self>{Ok(Box::new(T::load(r)?))}}
impl Wire for Range<usize>{fn save(&self,w:&mut impl crate::domain::code::cache::Output){self.start.save(w);self.end.save(w)}fn load(r:&mut &[u8])->io::Result<Self>{Ok(usize::load(r)?..usize::load(r)?)}}
impl<K: Wire + Hash + Ord, V: Wire> Wire for HashMap<K, V> {
    fn save(&self, out: &mut impl Output) {
        self.len().save(out);
        if out.overflowed() {
            return;
        }
        let mut entries: Vec<_> = self.iter().collect();
        entries.sort_unstable_by_key(|entry| entry.0);
        for (key, value) in entries {
            key.save(out);
            value.save(out);
        }
    }

    fn load(input: &mut &[u8]) -> io::Result<Self> {
        let n = usize::load(input)?;
        if n > input.len() {
            return Err(bad());
        }
        let mut map = HashMap::new();
        for _ in 0..n {
            map.insert(K::load(input)?, V::load(input)?);
        }
        Ok(map)
    }
}

impl<K: Wire + Hash + Ord> Wire for HashSet<K> {
    fn save(&self, out: &mut impl Output) {
        self.len().save(out);
        if out.overflowed() {
            return;
        }
        let mut items: Vec<_> = self.iter().collect();
        items.sort_unstable();
        for item in items {
            item.save(out);
        }
    }

    fn load(input: &mut &[u8]) -> io::Result<Self> {
        Ok(Vec::<K>::load(input)?.into_iter().collect())
    }
}
macro_rules! tuple{($($t:ident:$i:tt),+) =>{impl<$($t:Wire),+> Wire for ($($t,)+){fn save(&self,w:&mut impl crate::domain::code::cache::Output){$(self.$i.save(w);)+}fn load(r:&mut &[u8])->io::Result<Self>{Ok(($($t::load(r)?,)+))}}}}
tuple!(A:0,B:1);tuple!(A:0,B:1,C:2);tuple!(A:0,B:1,C:2,D:3);
macro_rules! record {($t:ident {$($f:ident),*})=>{impl $crate::domain::code::cache::Wire for $t{
fn save(&self,w:&mut impl crate::domain::code::cache::Output){$(self.$f.save(w);)*}
fn load(r:&mut &[u8])->std::io::Result<Self>{use $crate::domain::code::cache::Wire;Ok(Self{$($f:Wire::load(r)?),*})}
}}}
pub(super) use record;

const KINDS:&[&str]=&["associated_type","attribute","batch_label","block_declaration","block_label","build_stage","build_target","class_declaration","class_definition","class_name","class_specifier","const_item","constructor_declaration","css_variable","embedded_declaration","enum_declaration","enum_item","enum_name","enum_specifier","enum_variant","field_declaration","function_declaration","function_definition","function_expression","function_item","function_name","function_overload","function_signature_item","generator_function_declaration","generic_parameter","id_name","interface_declaration","label","lifetime_parameter","local_binding","macro_definition","message_name","method_declaration","method_definition","mod_item","module_declaration","object_reference","package_declaration","parameter","plpgsql_function","plpgsql_procedure","procedure_declaration","process_type","property_declaration","protocol_declaration","record_declaration","rpc_name","service_name","shell_assignment","source_file","static_item","struct_declaration","struct_item","struct_specifier","trait_item","type_alias_declaration","type_item","variable_declarator"];
impl Wire for &'static str {
    fn save(&self, out: &mut impl crate::domain::code::cache::Output) {
        self.len().save(out);
        out.extend_from_slice(self.as_bytes());
    }
    fn load(input: &mut &[u8]) -> io::Result<Self> {
        let length = usize::load(input)?;
        let (bytes, rest) = input.split_at_checked(length).ok_or_else(bad)?;
        let kind = std::str::from_utf8(bytes).map_err(|_| bad())?;
        let index = KINDS.binary_search(&kind).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, format!("unknown cached declaration kind {kind:?}"))
        })?;
        *input = rest;
        Ok(KINDS[index])
    }
}

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
    fn declaration_kinds_are_sorted_and_round_trip() {
        assert!(KINDS.windows(2).all(|w| w[0] < w[1]));
        for &kind in KINDS {
            let mut bytes = Vec::new();
            kind.save(&mut bytes);
            assert_eq!(<&'static str>::load(&mut bytes.as_slice()).unwrap(), kind);
        }
    }
}

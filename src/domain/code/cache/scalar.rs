//! Scalar encodings shared by cached analysis records.
use super::{Output, Wire, bad};
use std::{io, ops::Range};

impl Wire for usize {
    fn save(&self, out: &mut impl Output) {
        let mut value = *self;
        while value >= 128 {
            out.push((value as u8) | 128);
            value >>= 7;
        }
        out.push(value as u8);
    }

    fn load(input: &mut &[u8]) -> io::Result<Self> {
        let mut value = 0;
        for shift in (0..usize::BITS).step_by(7) {
            let (&byte, rest) = input.split_first().ok_or_else(bad)?;
            *input = rest;
            let part = payload_bits(byte, shift)?;
            value |= part;
            if byte < 128 {
                return Ok(value);
            }
        }
        Err(bad())
    }
}

fn payload_bits(byte: u8, shift: u32) -> io::Result<usize> {
    let part = usize::from(byte & 127);
    if part > usize::MAX >> shift {
        return Err(bad());
    }
    Ok(part << shift)
}

impl Wire for bool {
    fn save(&self, out: &mut impl Output) {
        out.push(*self as u8);
    }

    fn load(input: &mut &[u8]) -> io::Result<Self> {
        match usize::load(input)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(bad()),
        }
    }
}

fn save_text(text: &str, out: &mut impl Output) {
    text.len().save(out);
    out.extend_from_slice(text.as_bytes());
}

fn text_payload<'a>(input: &mut &'a [u8]) -> io::Result<(&'a [u8], &'a [u8])> {
    let length = usize::load(input)?;
    input.split_at_checked(length).ok_or_else(bad)
}

impl Wire for String {
    fn save(&self, out: &mut impl Output) {
        save_text(self, out);
    }

    fn load(input: &mut &[u8]) -> io::Result<Self> {
        let (bytes, rest) = text_payload(input)?;
        *input = rest;
        std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| bad())
    }
}

impl Wire for Range<usize> {
    fn save(&self, out: &mut impl Output) {
        self.start.save(out);
        self.end.save(out);
    }

    fn load(input: &mut &[u8]) -> io::Result<Self> {
        Ok(usize::load(input)?..usize::load(input)?)
    }
}

impl Wire for &'static str {
    fn save(&self, out: &mut impl Output) {
        save_text(self, out);
    }

    fn load(input: &mut &[u8]) -> io::Result<Self> {
        let (bytes, rest) = text_payload(input)?;
        let kind = std::str::from_utf8(bytes).map_err(|_| bad())?;
        let index = KINDS.binary_search(&kind).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown cached declaration kind {kind:?}"),
            )
        })?;
        *input = rest;
        Ok(KINDS[index])
    }
}

const KINDS: &[&str] = &[
    "associated_type",
    "attribute",
    "batch_label",
    "block_declaration",
    "block_label",
    "build_stage",
    "build_target",
    "class_declaration",
    "class_definition",
    "class_name",
    "class_specifier",
    "const_item",
    "constructor_declaration",
    "css_variable",
    "embedded_declaration",
    "enum_declaration",
    "enum_item",
    "enum_name",
    "enum_specifier",
    "enum_variant",
    "field_declaration",
    "function_declaration",
    "function_definition",
    "function_expression",
    "function_item",
    "function_name",
    "function_overload",
    "function_signature_item",
    "generator_function_declaration",
    "generic_parameter",
    "id_name",
    "interface_declaration",
    "label",
    "lifetime_parameter",
    "local_binding",
    "macro_definition",
    "message_name",
    "method_declaration",
    "method_definition",
    "mod_item",
    "module_declaration",
    "object_reference",
    "package_declaration",
    "parameter",
    "plpgsql_function",
    "plpgsql_procedure",
    "procedure_declaration",
    "process_type",
    "property_declaration",
    "protocol_declaration",
    "record_declaration",
    "rpc_name",
    "service_name",
    "shell_assignment",
    "source_file",
    "static_item",
    "struct_declaration",
    "struct_item",
    "struct_specifier",
    "trait_item",
    "type_alias_declaration",
    "type_item",
    "variable_declarator",
];

#[cfg(test)]
mod tests {
    use super::*;

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

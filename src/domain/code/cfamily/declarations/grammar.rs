//! File-level lexical and type rules for the four declaration dialects.
use super::super::overloads::CallGrammar;
use super::{Analyzer, C, Cpp, Csharp, Swift};
use crate::domain::code::Language;

const C: u8 = 1;
const CPP: u8 = 2;
const CSHARP: u8 = 4;
const SWIFT: u8 = 8;

// Spelling alone restricts a name only in the indicated grammars. Contextual
// uses such as a C++ final specifier are recognized at their source site.
fn reserved_in(word: &str) -> u8 {
    match word {
        "alignof" | "bool" | "delete" | "explicit" | "export" | "namespace" | "new" | "nullptr"
        | "protected" | "template" | "this" | "typename" | "using" | "virtual" => CPP | CSHARP,
        "as" | "extension" | "func" | "guard" | "import" | "in" | "init" | "internal" | "is"
        | "let" | "protocol" | "self" | "throws" | "var" => CSHARP | SWIFT,
        "auto" | "char" | "const" | "double" | "extern" | "float" | "int" | "long" | "register"
        | "short" | "signed" | "sizeof" | "typedef" | "union" | "unsigned" | "void"
        | "volatile" => C | CPP | CSHARP,
        "await" | "foreach" | "get" | "interface" | "null" | "out" | "override" | "record"
        | "ref" | "set" | "string" => CSHARP,
        "break" | "case" | "continue" | "default" | "do" | "else" | "enum" | "for" | "if"
        | "return" | "static" | "struct" | "switch" | "while" => C | CPP | CSHARP | SWIFT,
        "catch" | "class" | "false" | "operator" | "private" | "public" | "throw" | "true"
        | "try" => CPP | CSHARP | SWIFT,
        _ => 0,
    }
}

fn decimal_digits(text: &str) -> bool {
    text.bytes().all(|byte| byte.is_ascii_digit())
}

fn written_decimal(tokens: &[crate::domain::code::Token<'_>]) -> bool {
    match tokens {
        [whole, dot, fraction] if dot.text == "." => {
            [whole.text, fraction.text].into_iter().all(decimal_digits)
        }
        _ => false,
    }
}

pub(in crate::domain::code::cfamily) struct Grammar {
    dialect: u8,
    pub extract: fn(&mut Analyzer),
    pub generic_type_suffix: bool,
    pub incomplete_tag_usable: bool,
    pub forward_declarations_visible: bool,
    pub anonymous_namespace: bool,
    pub temporary_receiver: bool,
    pub argument_labels: bool,
    pub boolean: &'static str,
    pub integer: &'static str,
    pub decimal: &'static str,
    pub string: Option<&'static str>,
}

impl Grammar {
    pub(in crate::domain::code::cfamily) fn reserved(&self, word: &str) -> bool {
        reserved_in(word) & self.dialect != 0
    }
    pub(in crate::domain::code::cfamily) fn c_qualifiers(&self) -> bool {
        self.dialect & (C | CPP) != 0
    }
    pub(in crate::domain::code::cfamily) fn literal_type(
        &self,
        tokens: &[crate::domain::code::Token<'_>],
        digit_separators: bool,
    ) -> Option<&'static str> {
        let [token] = tokens else {
            return written_decimal(tokens).then_some(self.decimal);
        };
        match token.text {
            "true" | "false" => Some(self.boolean),
            text if token.kind == crate::domain::code::Kind::String && text.starts_with('"') => {
                self.string
            }
            text => self.integer_type(text, digit_separators),
        }
    }

    fn integer_type(&self, text: &str, digit_separators: bool) -> Option<&'static str> {
        if !text.starts_with(|ch: char| ch.is_ascii_digit()) {
            return None;
        }
        let native = self.c_qualifiers();
        let prefixed = native && text.starts_with('0');
        if !prefixed && decimal_digits(text) {
            return text.parse::<i32>().ok().map(|_| self.integer);
        }
        if !native {
            return None;
        }
        let n = super::super::number::Literal::read(text, digit_separators)?;
        // Every supported native data model can represent these unsuffixed
        // magnitudes as int. Wider hex and octal constants depend on the target.
        (n.suffix.is_empty() && n.value <= i16::MAX as u64).then_some(self.integer)
    }
    pub(in crate::domain::code::cfamily) fn cpp(&self) -> bool {
        self.dialect == CPP
    }
    const C: Self = Self {
        dialect: C,
        extract: |a| a.extract_dialect(C::default()),
        generic_type_suffix: false,
        incomplete_tag_usable: false,
        forward_declarations_visible: false,
        anonymous_namespace: false,
        temporary_receiver: false,
        argument_labels: false,
        boolean: "bool",
        integer: "int",
        decimal: "double",
        string: None,
    };
    pub(in crate::domain::code::cfamily) fn select(lang: Language) -> &'static Self {
        match lang {
            Language::C => &Self::C,
            Language::Cpp => &Self {
                dialect: CPP,
                extract: |a| a.extract_dialect(Cpp),
                anonymous_namespace: true,
                temporary_receiver: true,
                generic_type_suffix: true,
                forward_declarations_visible: true,
                string: Some("char*"),
                ..Self::C
            },
            Language::Csharp => &Self {
                dialect: CSHARP,
                extract: |a| a.extract_dialect(Csharp),
                generic_type_suffix: true,
                incomplete_tag_usable: true,
                forward_declarations_visible: true,
                string: Some("string"),
                ..Self::C
            },
            Language::Swift => &Self {
                dialect: SWIFT,
                extract: |a| a.extract_dialect(Swift),
                incomplete_tag_usable: true,
                forward_declarations_visible: true,
                argument_labels: Swift::ARGUMENT_LABELS,
                boolean: "Bool",
                integer: "Int",
                decimal: "Double",
                string: Some("String"),
                ..Self::C
            },
            _ => unreachable!("unsupported C-family dialect"),
        }
    }
}

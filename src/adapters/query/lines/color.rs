#[derive(Default)]
pub(super) struct Style {
    fg: Option<String>,
    bg: Option<String>,
    bold: bool,
    underline: bool,
    intense: bool,
}
impl Style {
    pub fn apply(&mut self, key: &str, value: &str) -> Result<(), String> {
        match key {
            "none" => *self = Self::default(),
            "style" => match value {
                "bold" | "nobold" => self.bold = value == "bold",
                "underline" | "nounderline" => self.underline = value == "underline",
                "intense" | "nointense" => self.intense = value == "intense",
                _ => {
                    return Err(format!(
                        "error parsing flag --colors: unrecognized style attribute '{value}'. Choose from: nobold, bold, nointense, intense, nounderline, underline."
                    ));
                }
            },
            "fg" | "bg" => {
                let suffix = if let Some(i) = [
                    "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
                ]
                .iter()
                .position(|&c| c == value)
                {
                    (if key == "fg" { 30 + i } else { 40 + i }).to_string()
                } else {
                    let nums = value
                        .split(',')
                        .map(|s| {
                            s.strip_prefix("0x")
                                .map_or_else(|| s.parse::<u8>(), |v| u8::from_str_radix(v, 16))
                        })
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|_| format!("invalid color: {value}"))?;
                    let plane = if key == "fg" { 38 } else { 48 };
                    match nums.as_slice() {
                        [n] => format!("{plane};5;{n}"),
                        [r, g, b] => format!("{plane};2;{r};{g};{b}"),
                        _ => return Err(format!("invalid color: {value}")),
                    }
                };
                if key == "fg" {
                    self.fg = Some(suffix)
                } else {
                    self.bg = Some(suffix)
                }
            }
            _ => return Err(format!("invalid color attribute: {key}")),
        }
        Ok(())
    }
    pub fn ansi(&self) -> String {
        let mut codes = String::new();
        if self.bold {
            codes.push_str("\x1b[1m");
        }
        if self.underline {
            codes.push_str("\x1b[4m");
        }
        for c in [&self.fg, &self.bg].into_iter().flatten() {
            let v = if self.intense {
                c.parse::<u8>().ok().map(|v| (v + 60).to_string())
            } else {
                None
            };
            codes.push_str(&format!("\x1b[{}m", v.as_ref().unwrap_or(c)));
        }
        codes
    }
}
pub(super) struct Colors {
    pub path: Style,
    pub line: Style,
    pub column: Style,
    pub matched: Style,
}
impl Default for Colors {
    fn default() -> Self {
        Self {
            path: Style {
                fg: Some("35".into()),
                ..Default::default()
            },
            line: Style {
                fg: Some("32".into()),
                ..Default::default()
            },
            column: Style::default(),
            matched: Style {
                fg: Some("31".into()),
                bold: true,
                ..Default::default()
            },
        }
    }
}
impl Colors {
    pub fn set(&mut self, s: &str) -> Result<(), String> {
        let mut parts = s.splitn(3, ':');
        let target = match parts.next() {
            Some("path") => &mut self.path,
            Some("line") => &mut self.line,
            Some("column") => &mut self.column,
            Some("match") => &mut self.matched,
            _ => return Err(format!("invalid color target: {s}")),
        };
        target.apply(parts.next().unwrap_or(""), parts.next().unwrap_or(""))
    }
}

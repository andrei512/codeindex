#[derive(Default)]
pub(super) struct Style {
    fg: Option<String>,
    bg: Option<String>,
    bold: bool,
    underline: bool,
    intense: bool,
}
#[derive(Clone, Copy)]
enum Plane {
    Foreground,
    Background,
}

impl Plane {
    fn color(self, value: &str) -> Result<String, String> {
        const COLORS: &[&str] = &["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"];
        let base = match self { Self::Foreground => 30, Self::Background => 40 };
        if let Some(index) = COLORS.iter().position(|&name| name == value) {
            return Ok((base + index).to_string());
        }
        let nums = value.split(',')
            .map(|part| part.strip_prefix("0x").map_or_else(|| part.parse::<u8>(), |hex| u8::from_str_radix(hex, 16)))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| format!("invalid color: {value}"))?;
        let plane = base + 8;
        match nums.as_slice() {
            [n] => Ok(format!("{plane};5;{n}")),
            [r, g, b] => Ok(format!("{plane};2;{r};{g};{b}")),
            _ => Err(format!("invalid color: {value}")),
        }
    }
}

impl Style {
    pub fn apply(&mut self, key: &str, value: &str) -> Result<(), String> {
        match key {
            "none" => *self = Self::default(),
            "style" => self.apply_style(value)?,
            "fg" => self.fg = Some(Plane::Foreground.color(value)?),
            "bg" => self.bg = Some(Plane::Background.color(value)?),
            _ => return Err(format!("invalid color attribute: {key}")),
        }
        Ok(())
    }

    fn apply_style(&mut self, value: &str) -> Result<(), String> {
        match value {
            "bold" | "nobold" => self.bold = value == "bold",
            "underline" | "nounderline" => self.underline = value == "underline",
            "intense" | "nointense" => self.intense = value == "intense",
            _ => return Err(format!(
                "error parsing flag --colors: unrecognized style attribute '{value}'. Choose from: nobold, bold, nointense, intense, nounderline, underline."
            )),
        }
        Ok(())
    }

    fn intense_color(&self, color: &str) -> Option<String> {
        if !self.intense {
            return None;
        }
        color.parse::<u8>().ok().map(|value| (value + 60).to_string())
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
            let intense = self.intense_color(c);
            let code = intense.as_ref().unwrap_or(c);
            codes.push_str(&format!("\x1b[{code}m"));
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

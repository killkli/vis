//! Typed terminal styles and the original UI's independent color/attribute merge.
//!
//! `parse` follows C's permissive definition API: unknown fields are ignored.
//! `validate` additionally reports malformed specifications to callers that want
//! diagnostics. Neither path retains input strings or emits them to a terminal.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Color {
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Attributes(u8);

impl Attributes {
    pub const UNDERLINE: Self = Self(1);
    pub const REVERSE: Self = Self(2);
    pub const BLINK: Self = Self(4);
    pub const BOLD: Self = Self(8);
    pub const ITALIC: Self = Self(16);
    pub const DIM: Self = Self(32);

    pub fn bits(self) -> u8 {
        self.0
    }
    pub fn contains(self, attribute: Self) -> bool {
        self.0 & attribute.0 != 0
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Style {
    pub attributes: Attributes,
    pub foreground: Option<Color>,
    pub background: Option<Color>,
    pub keep_attribute: bool,
}

/// Ordered, callback-time snapshots of native UI painting operations. Ranges
/// are half-open after converting the Lua API's inclusive finish position.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DrawOperation {
    Range {
        style: Style,
        range: std::ops::Range<usize>,
    },
    CoreOverlays {
        selection: Style,
        cursor: Style,
        primary: Style,
        eof: Style,
    },
    Cell {
        x: usize,
        y: usize,
        style: Style,
    },
    Status {
        left: Vec<u8>,
        right: Vec<u8>,
        style: Style,
    },
    BodyCopy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StyleError {
    pub offset: usize,
}

impl std::fmt::Display for StyleError {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(output, "invalid style token at byte {}", self.offset)
    }
}
impl std::error::Error for StyleError {}

impl Style {
    /// VT100 backend defaults in ui-terminal-vt100.c.
    pub fn terminal_default() -> Self {
        Self {
            foreground: Some(Color::Indexed(7)),
            background: Some(Color::Indexed(0)),
            ..Self::default()
        }
    }

    pub fn parse(specification: &str) -> Self {
        Self::parse_inner(specification).0
    }

    pub fn validate(specification: &str) -> Result<Self, StyleError> {
        let (style, error) = Self::parse_inner(specification);
        error.map_or(Ok(style), Err)
    }

    fn parse_inner(specification: &str) -> (Self, Option<StyleError>) {
        let mut style = Self::default();
        let mut error = None;
        let mut remaining = specification;
        while !remaining.is_empty() {
            remaining = remaining.trim_ascii_start();
            let offset = specification.len() - remaining.len();
            remaining = remaining.trim_ascii_end();
            let (token, rest) = remaining.split_once(',').unwrap_or((remaining, ""));
            remaining = rest;
            let (key, value) = token.split_once(':').unwrap_or((token, ""));
            let value = value.trim_ascii();
            let attribute = match key.to_ascii_lowercase().as_str() {
                "underlined" => Some(Attributes::UNDERLINE),
                "reverse" => Some(Attributes::REVERSE),
                "blink" => Some(Attributes::BLINK),
                "bold" => Some(Attributes::BOLD),
                "italics" => Some(Attributes::ITALIC),
                "dim" => Some(Attributes::DIM),
                _ => None,
            };
            if let Some(attribute) = attribute {
                style.attributes.0 |= attribute.0;
            } else if key.eq_ignore_ascii_case("keep_attribute") {
                style.keep_attribute = true;
            } else if key.eq_ignore_ascii_case("fore") || key.eq_ignore_ascii_case("back") {
                let color = parse_color(value);
                if key.eq_ignore_ascii_case("fore") {
                    style.foreground = color.or(style.foreground);
                } else {
                    style.background = color.or(style.background);
                }
                if !valid_color(value) {
                    error.get_or_insert(StyleError { offset });
                }
            } else if !key.is_empty() {
                error.get_or_insert(StyleError { offset });
            }
        }
        (style, error)
    }

    /// Colors inherit separately. Attributes replace the old set unless the
    /// overlay explicitly requests keep_attribute, as vis_cell_style_merge does.
    pub fn merge(self, overlay: Self) -> Self {
        Self {
            attributes: Attributes(
                overlay.attributes.0
                    | if overlay.keep_attribute {
                        self.attributes.0
                    } else {
                        0
                    },
            ),
            foreground: overlay.foreground.or(self.foreground),
            background: overlay.background.or(self.background),
            keep_attribute: overlay.keep_attribute,
        }
    }

    /// Emit a complete bounded state, clearing attributes from earlier cells.
    pub fn to_sgr(self) -> String {
        let mut parameters = Vec::new();
        for (attribute, parameter) in [
            (Attributes::BOLD, "1"),
            (Attributes::DIM, "2"),
            (Attributes::ITALIC, "3"),
            (Attributes::UNDERLINE, "4"),
            (Attributes::BLINK, "5"),
            (Attributes::REVERSE, "7"),
        ] {
            if self.attributes.contains(attribute) {
                parameters.push(parameter.to_owned());
            }
        }
        for (color, base) in [(self.foreground, 30), (self.background, 40)] {
            match color {
                Some(Color::Indexed(index)) if index < 8 => {
                    parameters.push((base + usize::from(index)).to_string())
                }
                Some(Color::Indexed(index)) => parameters.push(format!("{};5;{index}", base + 8)),
                Some(Color::Rgb(r, g, b)) => parameters.push(format!("{};2;{r};{g};{b}", base + 8)),
                None => {}
            }
        }
        if parameters.is_empty() {
            "\x1b[0m".to_owned()
        } else {
            format!("\x1b[0m\x1b[{}m", parameters.join(";"))
        }
    }
}

fn color_name(value: &str) -> Option<Color> {
    [
        "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
    ]
    .iter()
    .position(|name| value.eq_ignore_ascii_case(name))
    .map(|index| Color::Indexed(index as u8))
}

// The original integer_conversion reports success for a numeric prefix, even
// an empty prefix. Keep that documented compatibility quirk in the lossy parser.
fn integer_prefix(value: &str, radix: u32, auto_hex: bool) -> Option<u64> {
    let (negative, mut value) = value
        .strip_prefix('-')
        .map_or((false, value), |v| (true, v));
    if auto_hex {
        // C's automatic 0x check reads raw[1], so a leading minus prevents
        // that prefix from being recognized (a following '#' still works).
        value = if negative {
            value.strip_prefix('#').unwrap_or(value)
        } else {
            value
                .strip_prefix("0x")
                .or_else(|| value.strip_prefix("0X"))
                .or_else(|| value.strip_prefix('#'))
                .unwrap_or(value)
        };
    }
    let mut number = 0u64;
    for digit in value
        .bytes()
        .map(char::from)
        .map_while(|c| c.to_digit(radix))
    {
        number = number
            .checked_mul(u64::from(radix))?
            .checked_add(u64::from(digit))?;
    }
    Some(if negative {
        number.wrapping_neg()
    } else {
        number
    })
}

fn parse_color(value: &str) -> Option<Color> {
    if value.len() == 7 && value.starts_with('#') {
        let rgb = integer_prefix(&value[1..], 16, true)?;
        Some(Color::Rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8))
    } else if value.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        u8::try_from(integer_prefix(value, 10, false)?)
            .ok()
            .map(Color::Indexed)
    } else {
        color_name(value)
    }
}

fn valid_color(value: &str) -> bool {
    color_name(value).is_some()
        || value
            .strip_prefix('#')
            .is_some_and(|v| v.len() == 6 && v.bytes().all(|b| b.is_ascii_hexdigit()))
        || (!value.is_empty()
            && value.bytes().all(|b| b.is_ascii_digit())
            && value.parse::<u8>().is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attributes_replace_or_explicitly_accumulate_and_colors_inherit_separately() {
        let base = Style::parse("bold,italics,fore:red,back:blue");
        let overlay = Style::parse("underlined,fore:#010203");
        let merged = base.merge(overlay);
        assert_eq!(merged.attributes, Attributes::UNDERLINE);
        assert_eq!(merged.background, Some(Color::Indexed(4)));
        assert_eq!(merged.foreground, Some(Color::Rgb(1, 2, 3)));
        let kept = base.merge(Style::parse("keep_attribute,underlined"));
        assert!(kept.attributes.contains(Attributes::BOLD));
        assert!(kept.attributes.contains(Attributes::ITALIC));
        assert!(kept.attributes.contains(Attributes::UNDERLINE));
        assert_eq!(base.merge(Style::default()).attributes.bits(), 0);
    }

    #[test]
    fn c_parser_is_permissive_but_validation_reports_bad_tokens_without_emitting_input() {
        assert!(Style::validate("BoLd, fore: RED,back:255,keep_attribute").is_ok());
        for bad in [
            "fore:256",
            "nobold",
            "fore:red\x1b[2J",
            "back:default",
            "fore:#zzzzzz",
            "fore:12junk",
        ] {
            assert!(Style::validate(bad).is_err(), "{bad:?}");
            assert!(!Style::parse(bad).to_sgr().contains("2J"));
        }
        assert_eq!(
            Style::parse("fore:12junk").foreground,
            Some(Color::Indexed(12))
        );
        assert_eq!(
            Style::parse("fore:#zzzzzz").foreground,
            Some(Color::Rgb(0, 0, 0))
        );
        assert_eq!(
            Style::parse("fore:red,fore:invalid").foreground,
            Some(Color::Indexed(1))
        );
        assert_eq!(Style::parse("bold ,dim").attributes, Attributes::DIM);
    }

    #[test]
    fn complete_sgr_clears_previous_attributes_and_only_contains_generated_parameters() {
        assert_eq!(Style::parse("bold,fore:red").to_sgr(), "\x1b[0m\x1b[1;31m");
        assert_eq!(
            Style::parse("fore:#010203,back:200").to_sgr(),
            "\x1b[0m\x1b[38;2;1;2;3;48;5;200m"
        );
        assert_eq!(Style::default().to_sgr(), "\x1b[0m");
    }
}

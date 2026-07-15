use vtparse::CsiParam;

use crate::session::styled_line::Style;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AnsiColor {
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Color {
    Ansi { color: AnsiColor, bold: bool },
    Rgb { r: u8, g: u8, b: u8 },
    Echo,
    Output,
    Warn,
    /// The theme's default text color — distinct from ANSI white so light
    /// color schemes can render plain server text readably. Bold is carried
    /// orthogonally (SGR 1 brightens *default-ness* rather than collapsing
    /// onto a fixed palette slot), so themes decide what "bright default"
    /// looks like and `ESC[1;31m` still yields bright red.
    DefaultForeground { bold: bool },
    DefaultBackground,
}

impl Color {
    /// The bold/bright intensity carried by this foreground color, used when
    /// a subsequent SGR 30-37 inherits intensity per ECMA-48.
    #[must_use]
    pub const fn is_bold(self) -> bool {
        matches!(
            self,
            Self::Ansi { bold: true, .. } | Self::DefaultForeground { bold: true }
        )
    }
}

/// One semicolon-delimited SGR parameter together with its colon
/// sub-parameters, in order. `None` is an empty position, which ECMA-48
/// defines as the parameter's default (0).
type Slot = Vec<Option<i64>>;

/// Split a CSI parameter stream into [`Slot`]s: `;` separates slots, `:`
/// separates sub-parameters within a slot, and an integer fills the current
/// position. An empty stream yields one empty slot, so `CSI m` naturally
/// means `CSI 0 m`. Returns `None` for streams carrying parameter bytes that
/// are not SGR separators (private-marker sequences are not SGR).
fn split_slots(params: &[CsiParam]) -> Option<Vec<Slot>> {
    let mut slots: Vec<Slot> = Vec::new();
    let mut current: Slot = Vec::new();
    let mut position_filled = false;
    for param in params {
        match param {
            CsiParam::Integer(n) => {
                current.push(Some(*n));
                position_filled = true;
            }
            CsiParam::P(b';') => {
                if !position_filled {
                    current.push(None);
                }
                slots.push(std::mem::take(&mut current));
                position_filled = false;
            }
            CsiParam::P(b':') => {
                if !position_filled {
                    current.push(None);
                }
                position_filled = false;
            }
            CsiParam::P(_) => return None,
        }
    }
    if !position_filled {
        current.push(None);
    }
    slots.push(current);
    Some(slots)
}

const fn ansi_color(index: i64) -> AnsiColor {
    match index {
        0 => AnsiColor::Black,
        1 => AnsiColor::Red,
        2 => AnsiColor::Green,
        3 => AnsiColor::Yellow,
        4 => AnsiColor::Blue,
        5 => AnsiColor::Magenta,
        6 => AnsiColor::Cyan,
        _ => AnsiColor::White,
    }
}

/// Clamp an SGR color component to the u8 range (out-of-range values clamp
/// rather than wrap — `38;2;300;0;0` is a saturated red, not a dim one).
fn component(value: i64) -> u8 {
    u8::try_from(value.clamp(0, 255)).unwrap_or(255)
}

/// Map a 256-color palette index: 0-15 the named colors, 16-231 the 6×6×6
/// cube, 232-255 the grayscale ramp. Out-of-range indexes clamp.
fn color_256(index: i64) -> Color {
    match index.clamp(0, 255) {
        n @ 16..=231 => {
            #[allow(clippy::cast_precision_loss)]
            let n = (n - 16) as f32;
            let r = (n / 36.0).floor();
            let g = ((n - (r * 36.0)) / 6.0).floor();
            let b = n - (r * 36.0) - (g * 6.0);
            let mul = 255.0 / 6.0;

            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            Color::Rgb {
                r: (r * mul).round() as u8,
                g: (g * mul).round() as u8,
                b: (b * mul).round() as u8,
            }
        }
        n @ 232..=255 => {
            let range = 255.0 / (255.0 - 232.0);
            #[allow(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss
            )]
            let val = (range * (n - 232) as f32).round() as u8;
            Color::Rgb {
                r: val,
                g: val,
                b: val,
            }
        }
        n @ 0..=7 => Color::Ansi {
            color: ansi_color(n),
            bold: false,
        },
        n => Color::Ansi {
            color: ansi_color(n - 8),
            bold: true,
        },
    }
}

/// Decode a colon-form extended color (`38:5:n`, `38:2::r:g:b`) from a slot's
/// sub-parameters (everything after the 38/48). Four or more components after
/// mode 2 mean the first is an ITU T.416 colorspace id, which is skipped.
/// `None` when the mode is missing or unrecognized.
fn extended_color_from_subparams(sub: &[Option<i64>]) -> Option<Color> {
    match sub.first().copied().flatten() {
        Some(5) => Some(color_256(sub.get(1).copied().flatten().unwrap_or(0))),
        Some(2) => {
            let comps = &sub[1..];
            let pick = |idx: usize| comps.get(idx).copied().flatten().unwrap_or(0);
            let offset = usize::from(comps.len() >= 4);
            Some(Color::Rgb {
                r: component(pick(offset)),
                g: component(pick(offset + 1)),
                b: component(pick(offset + 2)),
            })
        }
        _ => None,
    }
}

/// Decode a semicolon-form extended color (`38;5;n`, `38;2;r;g;b`) from the
/// slots following the 38/48 introducer. Returns the color (if the mode is
/// recognized) and how many following slots the directive consumed; a
/// truncated tail reads missing components as 0, matching common client
/// behavior.
fn extended_color_from_slots(rest: &[Slot]) -> (Option<Color>, usize) {
    let value = |idx: usize| {
        rest.get(idx)
            .and_then(|slot| slot.first().copied().flatten())
    };
    match value(0) {
        Some(5) => (Some(color_256(value(1).unwrap_or(0))), 2),
        Some(2) => (
            Some(Color::Rgb {
                r: component(value(1).unwrap_or(0)),
                g: component(value(2).unwrap_or(0)),
                b: component(value(3).unwrap_or(0)),
            }),
            4,
        ),
        _ => (None, 0),
    }
}

const fn brightened(fg: Color) -> Color {
    match fg {
        Color::Ansi { color, .. } => Color::Ansi { color, bold: true },
        Color::DefaultForeground { .. } => Color::DefaultForeground { bold: true },
        other => other,
    }
}

const fn dimmed(fg: Color) -> Color {
    match fg {
        Color::Ansi { color, .. } => Color::Ansi { color, bold: false },
        Color::DefaultForeground { .. } => Color::DefaultForeground { bold: false },
        other => other,
    }
}

/// Interprets one SGR (`CSI … m`) parameter list against `initial_style`,
/// returning the style the terminal cursor carries afterward.
///
/// Parameters apply independently, left to right, per ECMA-48: a directive
/// with no `Style` representation (underline, italic, blink, …) or an
/// unknown code skips only itself, so `ESC[1;4;31m` still yields bright red.
/// An empty parameter is the directive's default — in particular `ESC[m`
/// resets. Only sequences carrying non-SGR parameter bytes (private markers)
/// leave the style entirely untouched.
#[must_use]
pub fn process(initial_style: Style, params: &[CsiParam]) -> Style {
    let Some(slots) = split_slots(params) else {
        return initial_style;
    };

    let mut style = initial_style;
    let mut i = 0;
    while i < slots.len() {
        let slot = &slots[i];
        let code = slot.first().copied().flatten().unwrap_or(0);

        if slot.len() > 1 {
            // Colon form: the directive is self-contained in its slot. Only
            // extended colors have a Style representation; other colon
            // directives (underline styles, …) are recognized shapes with
            // nothing to apply.
            if (code == 38 || code == 48)
                && let Some(color) = extended_color_from_subparams(&slot[1..])
            {
                if code == 38 {
                    style.fg = color;
                } else {
                    style.bg = color;
                }
            }
            i += 1;
            continue;
        }

        match code {
            0 => {
                style = Style {
                    fg: Color::DefaultForeground { bold: false },
                    bg: Color::DefaultBackground,
                };
            }
            1 => style.fg = brightened(style.fg),
            // Faint has no distinct representation; like SGR 22 it clears
            // the intensity bit.
            2 | 22 => style.fg = dimmed(style.fg),
            30..=37 => {
                style.fg = Color::Ansi {
                    color: ansi_color(code - 30),
                    bold: style.fg.is_bold(),
                };
            }
            38 | 48 => {
                let (color, consumed) = extended_color_from_slots(&slots[i + 1..]);
                if let Some(color) = color {
                    if code == 38 {
                        style.fg = color;
                    } else {
                        style.bg = color;
                    }
                }
                i += consumed;
            }
            // 39 resets the color, not the intensity (ECMA-48).
            39 => {
                style.fg = Color::DefaultForeground {
                    bold: style.fg.is_bold(),
                };
            }
            40..=47 => {
                style.bg = Color::Ansi {
                    color: ansi_color(code - 40),
                    bold: false,
                };
            }
            49 => style.bg = Color::DefaultBackground,
            90..=97 => {
                style.fg = Color::Ansi {
                    color: ansi_color(code - 90),
                    bold: true,
                };
            }
            100..=107 => {
                style.bg = Color::Ansi {
                    color: ansi_color(code - 100),
                    bold: true,
                };
            }
            // Everything else — attributes without a Style representation
            // (3-9, 21, 23-29, 53, 55, …) and unknown codes — skips only
            // its own slot.
            _ => {}
        }
        i += 1;
    }
    style
}

#[cfg(test)]
mod tests {
    use super::{AnsiColor, Color, process};
    use crate::session::styled_line::Style;
    use vtparse::CsiParam;

    /// Build a `CsiParam` stream from the text between `CSI` and `m`,
    /// mirroring vtparse's shape: an `Integer` per number, a `P` per
    /// separator.
    fn params(s: &str) -> Vec<CsiParam> {
        let mut out = Vec::new();
        let mut num: Option<i64> = None;
        for ch in s.chars() {
            match ch {
                '0'..='9' => {
                    num = Some(num.unwrap_or(0) * 10 + i64::from(ch as u8 - b'0'));
                }
                ';' | ':' => {
                    if let Some(n) = num.take() {
                        out.push(CsiParam::Integer(n));
                    }
                    out.push(CsiParam::P(ch as u8));
                }
                _ => panic!("unexpected char {ch:?} in SGR test params"),
            }
        }
        if let Some(n) = num {
            out.push(CsiParam::Integer(n));
        }
        out
    }

    fn default_style() -> Style {
        Style {
            fg: Color::DefaultForeground { bold: false },
            bg: Color::DefaultBackground,
        }
    }

    fn apply(initial: Style, s: &str) -> Style {
        process(initial, &params(s))
    }

    const RED: Color = Color::Ansi {
        color: AnsiColor::Red,
        bold: false,
    };
    const BRIGHT_RED: Color = Color::Ansi {
        color: AnsiColor::Red,
        bold: true,
    };

    #[test]
    fn empty_list_resets() {
        let loud = Style {
            fg: BRIGHT_RED,
            bg: Color::Ansi {
                color: AnsiColor::Blue,
                bold: false,
            },
        };
        assert_eq!(process(loud, &[]), default_style());
    }

    #[test]
    fn unsupported_attribute_does_not_poison_colors() {
        let got = apply(default_style(), "1;4;31");
        assert_eq!(got.fg, BRIGHT_RED);
        assert_eq!(got.bg, Color::DefaultBackground);
    }

    #[test]
    fn unknown_code_skips_only_itself() {
        let got = apply(default_style(), "7;31;53");
        assert_eq!(got.fg, RED);
    }

    #[test]
    fn background_colors() {
        assert_eq!(
            apply(default_style(), "41").bg,
            Color::Ansi {
                color: AnsiColor::Red,
                bold: false
            }
        );
        assert_eq!(
            apply(default_style(), "101").bg,
            Color::Ansi {
                color: AnsiColor::Red,
                bold: true
            }
        );
        let cleared = apply(apply(default_style(), "41"), "49");
        assert_eq!(cleared.bg, Color::DefaultBackground);
    }

    #[test]
    fn extended_background_semicolon_forms() {
        assert_eq!(
            apply(default_style(), "48;2;10;20;30").bg,
            Color::Rgb {
                r: 10,
                g: 20,
                b: 30
            }
        );
        assert_eq!(
            apply(default_style(), "48;5;9").bg,
            Color::Ansi {
                color: AnsiColor::Red,
                bold: true
            }
        );
    }

    #[test]
    fn truecolor_colon_forms() {
        // With and without the ITU T.416 colorspace-id slot.
        let want = Color::Rgb { r: 255, g: 0, b: 0 };
        assert_eq!(apply(default_style(), "38:2::255:0:0").fg, want);
        assert_eq!(apply(default_style(), "38:2:255:0:0").fg, want);
        assert_eq!(apply(default_style(), "38:5:9").fg, BRIGHT_RED);
    }

    #[test]
    fn extended_color_consumes_its_slots() {
        // The 31 after a semicolon-form truecolor must apply, not be eaten.
        let got = apply(default_style(), "48;2;1;2;3;31");
        assert_eq!(got.fg, RED);
        assert_eq!(got.bg, Color::Rgb { r: 1, g: 2, b: 3 });
    }

    #[test]
    fn out_of_range_components_clamp() {
        assert_eq!(
            apply(default_style(), "38;2;300;0;0").fg,
            Color::Rgb { r: 255, g: 0, b: 0 }
        );
        assert_eq!(
            apply(default_style(), "38;5;999").fg,
            Color::Rgb {
                r: 255,
                g: 255,
                b: 255
            }
        );
    }

    #[test]
    fn truncated_extended_color_reads_zeroes() {
        assert_eq!(
            apply(default_style(), "38;2;255").fg,
            Color::Rgb { r: 255, g: 0, b: 0 }
        );
        assert_eq!(
            apply(default_style(), "38;5").fg,
            Color::Ansi {
                color: AnsiColor::Black,
                bold: false
            }
        );
    }

    #[test]
    fn bare_extended_introducer_is_skipped() {
        assert_eq!(apply(default_style(), "38"), default_style());
        // An unrecognized mode consumes nothing: the 41 still applies.
        assert_eq!(
            apply(default_style(), "38;41").bg,
            Color::Ansi {
                color: AnsiColor::Red,
                bold: false
            }
        );
    }

    #[test]
    fn empty_params_are_resets() {
        let red = apply(default_style(), "31");
        assert_eq!(apply(red, ";31").fg, RED);
        assert_eq!(apply(red, "31;"), default_style());
    }

    #[test]
    fn faint_and_normal_intensity_clear_bold() {
        let bright = apply(default_style(), "1;31");
        assert_eq!(bright.fg, BRIGHT_RED);
        assert_eq!(apply(bright, "2").fg, RED);
        assert_eq!(apply(bright, "22").fg, RED);
    }

    #[test]
    fn color_inherits_intensity_per_ecma() {
        let got = apply(default_style(), "1;33");
        assert_eq!(
            got.fg,
            Color::Ansi {
                color: AnsiColor::Yellow,
                bold: true
            }
        );
        // 39 keeps intensity; a later 30-37 inherits it.
        let kept = apply(got, "39");
        assert_eq!(kept.fg, Color::DefaultForeground { bold: true });
    }

    #[test]
    fn cube_and_grayscale_mapping() {
        assert_eq!(
            apply(default_style(), "38;5;196").fg,
            Color::Rgb { r: 213, g: 0, b: 0 }
        );
        assert_eq!(
            apply(default_style(), "38;5;232").fg,
            Color::Rgb { r: 0, g: 0, b: 0 }
        );
        assert_eq!(
            apply(default_style(), "38;5;7").fg,
            Color::Ansi {
                color: AnsiColor::White,
                bold: false
            }
        );
    }

    #[test]
    fn private_marker_sequences_leave_style_untouched() {
        let red = Style {
            fg: RED,
            bg: Color::DefaultBackground,
        };
        let stream = [CsiParam::P(b'?'), CsiParam::Integer(25)];
        assert_eq!(process(red, &stream), red);
    }
}

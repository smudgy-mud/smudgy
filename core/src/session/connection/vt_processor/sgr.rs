use smallvec::SmallVec;
use vtparse::CsiParam;

use crate::session::styled_line::{Blink, Style, Underline};

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
    Ansi {
        color: AnsiColor,
        /// Selects the bright half of the ANSI palette. SGR bold font weight
        /// is carried independently by `Style`.
        bold: bool,
    },
    Rgb {
        r: u8,
        g: u8,
        b: u8,
    },
    Echo,
    Output,
    Warn,
    /// The theme's default text color — distinct from ANSI white so light
    /// color schemes can render plain server text readably. `bold` selects the
    /// bright-default palette value; SGR font weight lives independently in
    /// `Style`.
    DefaultForeground {
        bold: bool,
    },
    DefaultBackground,
}

/// Inline capacity for one SGR sequence's parameter positions. `vtparse` caps
/// a CSI at 32 parameters (integers and separators combined), so even the
/// worst case — 32 separators yielding 33 empty positions — fits without
/// spilling; a larger stream falls back to the heap rather than truncating.
const INLINE_POSITIONS: usize = 40;

/// One SGR parameter list split into semicolon-delimited slots, each holding
/// its colon sub-parameters in order. Positions are stored flat with a
/// per-slot end offset, so splitting a sequence allocates nothing on the
/// inbound parse path. `None` is an empty position, which ECMA-48 defines as
/// the parameter's default (0).
struct Slots {
    positions: SmallVec<[Option<i64>; INLINE_POSITIONS]>,
    /// Exclusive end offset of each slot into `positions`.
    ends: SmallVec<[usize; INLINE_POSITIONS]>,
}

impl Slots {
    fn len(&self) -> usize {
        self.ends.len()
    }

    /// Slot `index`'s positions: the parameter first, then its sub-parameters.
    fn slot(&self, index: usize) -> &[Option<i64>] {
        let start = if index == 0 { 0 } else { self.ends[index - 1] };
        &self.positions[start..self.ends[index]]
    }

    /// Slot `index`'s parameter value, when the slot exists and is non-empty.
    fn value(&self, index: usize) -> Option<i64> {
        (index < self.len())
            .then(|| self.slot(index))
            .and_then(|slot| slot.first().copied().flatten())
    }
}

/// Split a CSI parameter stream into [`Slots`]: `;` separates slots, `:`
/// separates sub-parameters within a slot, and an integer fills the current
/// position. An empty stream yields one empty slot, so `CSI m` naturally
/// means `CSI 0 m`. Returns `None` for streams carrying parameter bytes that
/// are not SGR separators (private-marker sequences are not SGR).
fn split_slots(params: &[CsiParam]) -> Option<Slots> {
    let mut slots = Slots {
        positions: SmallVec::new(),
        ends: SmallVec::new(),
    };
    let mut position_filled = false;
    for param in params {
        match param {
            CsiParam::Integer(n) => {
                slots.positions.push(Some(*n));
                position_filled = true;
            }
            CsiParam::P(b';') => {
                if !position_filled {
                    slots.positions.push(None);
                }
                slots.ends.push(slots.positions.len());
                position_filled = false;
            }
            CsiParam::P(b':') => {
                if !position_filled {
                    slots.positions.push(None);
                }
                position_filled = false;
            }
            CsiParam::P(_) => return None,
        }
    }
    if !position_filled {
        slots.positions.push(None);
    }
    slots.ends.push(slots.positions.len());
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
            let n = n - 16;
            let component = |level: i64| {
                // Xterm deliberately gives the first non-zero cube level a
                // larger step so dark colors remain distinguishable from a
                // black terminal background.
                u8::try_from(if level == 0 { 0 } else { 55 + 40 * level }).unwrap_or(255)
            };
            Color::Rgb {
                r: component(n / 36),
                g: component((n % 36) / 6),
                b: component(n % 6),
            }
        }
        n @ 232..=255 => {
            // Xterm's grayscale ramp spans 8..=238 in steps of ten, leaving
            // the ANSI black/white slots to represent the endpoints.
            let val = u8::try_from(8 + 10 * (n - 232)).unwrap_or(238);
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
/// slots following the 38/48 introducer at `slots[from..]`. Returns the color
/// (if the mode is recognized) and how many following slots the directive
/// consumed; a truncated tail reads missing components as 0, matching common
/// client behavior.
fn extended_color_from_slots(slots: &Slots, from: usize) -> (Option<Color>, usize) {
    let value = |idx: usize| slots.value(from + idx);
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

/// Interprets one SGR (`CSI … m`) parameter list against `initial_style`,
/// returning the style the terminal cursor carries afterward.
///
/// Parameters apply independently, left to right, per ECMA-48: a directive
/// with no `Style` representation or an unknown code skips only itself.
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
        let slot = slots.slot(i);
        let code = slot.first().copied().flatten().unwrap_or(0);

        if slot.len() > 1 {
            // Colon form: the directive is self-contained in its slot. Only
            // extended colors and the standard underline variants currently
            // have a Style representation.
            if (code == 38 || code == 48)
                && let Some(color) = extended_color_from_subparams(&slot[1..])
            {
                if code == 38 {
                    style.fg = color;
                } else {
                    style.bg = color;
                }
            } else if code == 4 {
                style.attributes.underline = match slot.get(1).copied().flatten().unwrap_or(1) {
                    0 => Underline::None,
                    2 => Underline::Double,
                    _ => Underline::Single,
                };
            }
            i += 1;
            continue;
        }

        match code {
            0 => {
                style = Style::default();
            }
            1 => {
                style.attributes.bold = true;
                style.attributes.faint = false;
            }
            2 => {
                style.attributes.bold = false;
                style.attributes.faint = true;
            }
            3 => style.attributes.italic = true,
            4 => style.attributes.underline = Underline::Single,
            5 => style.attributes.blink = Blink::Slow,
            6 => style.attributes.blink = Blink::Fast,
            7 => style.attributes.reverse = true,
            9 => style.attributes.crossed_out = true,
            21 => style.attributes.underline = Underline::Double,
            22 => {
                style.attributes.bold = false;
                style.attributes.faint = false;
            }
            23 => style.attributes.italic = false,
            24 => style.attributes.underline = Underline::None,
            25 => style.attributes.blink = Blink::None,
            27 => style.attributes.reverse = false,
            29 => style.attributes.crossed_out = false,
            30..=37 => {
                style.fg = Color::Ansi {
                    color: ansi_color(code - 30),
                    bold: false,
                };
            }
            38 | 48 => {
                let (color, consumed) = extended_color_from_slots(&slots, i + 1);
                if let Some(color) = color {
                    if code == 38 {
                        style.fg = color;
                    } else {
                        style.bg = color;
                    }
                }
                i += consumed;
            }
            // 39 resets the color, not the text intensity (ECMA-48).
            39 => style.fg = Color::DefaultForeground { bold: false },
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
            // Unsupported attributes and unknown codes skip only their own slot.
            _ => {}
        }
        i += 1;
    }
    style
}

#[cfg(test)]
mod tests {
    use super::{AnsiColor, Color, color_256, process};
    use crate::session::styled_line::{Blink, Style, TextAttributes, Underline};
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
        Style::DEFAULT
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
            ..Style::DEFAULT
        };
        assert_eq!(process(loud, &[]), default_style());
    }

    #[test]
    fn text_attributes_do_not_poison_colors() {
        let got = apply(default_style(), "1;4;31");
        assert_eq!(got.fg, RED);
        assert_eq!(got.bg, Color::DefaultBackground);
        assert!(got.attributes.bold);
        assert_eq!(got.attributes.underline, Underline::Single);
    }

    #[test]
    fn unknown_code_skips_only_itself() {
        let got = apply(default_style(), "7;31;53");
        assert_eq!(got.fg, RED);
        assert!(got.attributes.reverse);
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
                r: 238,
                g: 238,
                b: 238
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
    fn bold_faint_and_normal_intensity_are_distinct_attributes() {
        let bold = apply(default_style(), "1;31");
        assert_eq!(bold.fg, RED);
        assert!(bold.attributes.bold);
        assert!(!bold.attributes.faint);

        let faint = apply(bold, "2");
        assert_eq!(faint.fg, RED);
        assert!(!faint.attributes.bold);
        assert!(faint.attributes.faint);

        let normal = apply(faint, "22");
        assert_eq!(normal.fg, RED);
        assert!(!normal.attributes.bold);
        assert!(!normal.attributes.faint);
    }

    #[test]
    fn bold_is_independent_from_palette_brightness() {
        let got = apply(default_style(), "1;33");
        assert_eq!(
            got.fg,
            Color::Ansi {
                color: AnsiColor::Yellow,
                bold: false
            }
        );
        assert!(got.attributes.bold);
        // 39 resets only the color; the bold text attribute survives.
        let kept = apply(got, "39");
        assert_eq!(kept.fg, Color::DefaultForeground { bold: false });
        assert!(kept.attributes.bold);

        // Explicit bright color remains bright when normal intensity clears
        // the font-weight attribute.
        let explicit = apply(default_style(), "1;91;22");
        assert_eq!(explicit.fg, BRIGHT_RED);
        assert!(!explicit.attributes.bold);
    }

    #[test]
    fn supported_attributes_set_and_reset_independently() {
        let styled = apply(default_style(), "1;3;4;5;7;9");
        assert_eq!(
            styled.attributes,
            TextAttributes {
                bold: true,
                italic: true,
                underline: Underline::Single,
                blink: Blink::Slow,
                crossed_out: true,
                reverse: true,
                ..TextAttributes::DEFAULT
            }
        );

        let reset = apply(styled, "22;23;24;25;27;29");
        assert_eq!(reset.attributes, TextAttributes::DEFAULT);
    }

    #[test]
    fn fast_blink_and_double_underline_replace_related_modes() {
        let styled = apply(default_style(), "5;6;4;21");
        assert_eq!(styled.attributes.blink, Blink::Fast);
        assert_eq!(styled.attributes.underline, Underline::Double);

        assert_eq!(
            apply(default_style(), "4:2").attributes.underline,
            Underline::Double
        );
        assert_eq!(apply(styled, "4:0").attributes.underline, Underline::None);
    }

    #[test]
    fn cube_and_grayscale_mapping() {
        assert_eq!(
            apply(default_style(), "38;5;196").fg,
            Color::Rgb { r: 255, g: 0, b: 0 }
        );
        assert_eq!(
            apply(default_style(), "38;5;232").fg,
            Color::Rgb { r: 8, g: 8, b: 8 }
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
    fn cube_uses_xterm_component_levels() {
        let levels = [0, 95, 135, 175, 215, 255];
        for (level, expected) in levels.into_iter().enumerate() {
            let index = 16 + i64::try_from(level).unwrap_or(0) * 36;
            assert_eq!(
                color_256(index),
                Color::Rgb {
                    r: expected,
                    g: 0,
                    b: 0,
                }
            );
        }
        assert_eq!(
            color_256(231),
            Color::Rgb {
                r: 255,
                g: 255,
                b: 255,
            }
        );
        assert_eq!(
            color_256(255),
            Color::Rgb {
                r: 238,
                g: 238,
                b: 238,
            }
        );
    }

    #[test]
    fn private_marker_sequences_leave_style_untouched() {
        let red = Style {
            fg: RED,
            bg: Color::DefaultBackground,
            ..Style::DEFAULT
        };
        let stream = [CsiParam::P(b'?'), CsiParam::Integer(25)];
        assert_eq!(process(red, &stream), red);
    }
}

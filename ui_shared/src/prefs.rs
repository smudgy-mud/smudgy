//! Hot-swappable terminal preferences consumed by the shared terminal widgets.

use std::sync::{Arc, LazyLock};

use arc_swap::ArcSwap;
use iced::{Background, Color, Font};
use smudgy_session_model::{AnsiColor, Color as VtColor};

use crate::assets::GEIST_MONO;

pub use smudgy_session_model::preferences::{CommandInputBehavior, TerminalBoldMode};

#[derive(Clone, PartialEq)]
pub struct TerminalPalette {
    pub ansi: [Color; 16],
    pub foreground: Color,
    pub background: Color,
    pub echo: Color,
    pub warn: Color,
    pub output: Color,
    pub selection: Color,
    pub input_background: Color,
}

/// A selectable terminal palette plus the metadata needed to derive Smudgy's
/// application chrome. The color data is shared by native and browser; font
/// discovery and persistence remain host concerns.
#[derive(Clone, PartialEq)]
pub struct NamedTerminalPalette {
    pub name: &'static str,
    pub render: TerminalPalette,
    /// Accent for the app theme; `None` falls back to the foreground.
    pub accent: Option<Color>,
    /// Whether app chrome follows this palette instead of the stock theme.
    pub derive_app_theme: bool,
}

impl std::ops::Deref for NamedTerminalPalette {
    type Target = TerminalPalette;

    fn deref(&self) -> &Self::Target {
        &self.render
    }
}

impl std::ops::DerefMut for NamedTerminalPalette {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.render
    }
}

#[must_use]
pub fn palettes() -> &'static [&'static NamedTerminalPalette] {
    &crate::palettes::ALL
}

/// Looks a palette up by name, falling back to the stock Smudgy scheme.
#[must_use]
pub fn palette_by_name(name: &str) -> &'static NamedTerminalPalette {
    palettes()
        .iter()
        .find(|palette| palette.name.eq_ignore_ascii_case(name))
        .copied()
        .unwrap_or(&crate::palettes::SMUDGY)
}

/// Derive readable application chrome from a terminal palette.
#[must_use]
pub fn app_theme_for_palette(palette: &NamedTerminalPalette) -> smudgy_theme::Theme {
    let mut theme = smudgy_theme::smudgy();
    if !palette.derive_app_theme {
        theme.styles.general.background = palette.background;
        theme.styles.general.input_background = palette.input_background;
        return theme;
    }

    let bg = palette.background;
    let fg = palette.foreground;
    let mix = |a: Color, b: Color, amount: f32| Color {
        r: (b.r - a.r).mul_add(amount, a.r),
        g: (b.g - a.g).mul_add(amount, a.g),
        b: (b.b - a.b).mul_add(amount, a.b),
        a: a.a,
    };

    theme.styles.general.background = bg;
    theme.styles.general.container_background = mix(bg, fg, 0.05);
    theme.styles.general.border = mix(bg, fg, 0.18);
    theme.styles.general.rule = mix(bg, fg, 0.14);
    theme.styles.general.overlay_background = Color { a: 0.9, ..bg };
    theme.styles.general.accent = palette.accent.unwrap_or(fg);
    theme.styles.text.normal = fg;
    theme.styles.text.success = palette.ansi[2];
    theme.styles.text.error = palette.ansi[1];
    theme.styles.general.input_background = palette.input_background;
    theme.styles.general.input_text = fg;
    theme.styles.modal.title_bar_background = Background::Color(mix(bg, fg, 0.10));
    theme.styles.modal.body_background = Background::Color(mix(bg, fg, 0.04));
    theme
}

// Color resolution is on every freshly baked terminal span. Keep these bodies
// available to the native UI optimizer across the ui_shared crate boundary.
impl TerminalPalette {
    #[must_use]
    #[inline]
    pub fn resolve(&self, color: VtColor, theme_extended_colors: bool) -> Color {
        match color {
            VtColor::Ansi { color, bold } => {
                let base = match color {
                    AnsiColor::Black => 0,
                    AnsiColor::Red => 1,
                    AnsiColor::Green => 2,
                    AnsiColor::Yellow => 3,
                    AnsiColor::Blue => 4,
                    AnsiColor::Magenta => 5,
                    AnsiColor::Cyan => 6,
                    AnsiColor::White => 7,
                };
                self.ansi[base + usize::from(bold) * 8]
            }
            VtColor::Rgb { r, g, b } => {
                if theme_extended_colors {
                    self.archetypal(r, g, b)
                } else {
                    Color::from_rgb8(r, g, b)
                }
            }
            VtColor::Echo => self.echo,
            VtColor::Output => self.output,
            VtColor::Warn => self.warn,
            VtColor::DefaultForeground { bold } => {
                if bold {
                    self.bright_default()
                } else {
                    self.foreground
                }
            }
            VtColor::DefaultBackground => Color::TRANSPARENT,
        }
    }

    #[inline]
    fn bright_default(&self) -> Color {
        let bright = self.ansi[15];
        let distance = (bright.r - self.background.r).abs()
            + (bright.g - self.background.g).abs()
            + (bright.b - self.background.b).abs();
        if distance < 0.3 {
            self.foreground
        } else {
            bright
        }
    }

    #[must_use]
    #[inline]
    pub fn archetypal(&self, r: u8, g: u8, b: u8) -> Color {
        let red = f32::from(r) / 255.0;
        let green = f32::from(g) / 255.0;
        let blue = f32::from(b) / 255.0;
        let background = Oklab::from_srgb(self.background);
        let black_red = background.mix(Oklab::from_srgb(self.ansi[9]), red);
        let green_yellow =
            Oklab::from_srgb(self.ansi[10]).mix(Oklab::from_srgb(self.ansi[11]), red);
        let blue_magenta =
            Oklab::from_srgb(self.ansi[12]).mix(Oklab::from_srgb(self.ansi[13]), red);
        let cyan_white =
            Oklab::from_srgb(self.ansi[14]).mix(Oklab::from_srgb(self.bright_default()), red);
        black_red
            .mix(green_yellow, green)
            .mix(blue_magenta.mix(cyan_white, green), blue)
            .into_srgb()
    }
}

#[derive(Clone, Copy)]
struct Oklab {
    lightness: f32,
    green_red: f32,
    blue_yellow: f32,
}

impl Oklab {
    #[allow(clippy::excessive_precision)]
    #[inline]
    fn from_srgb(color: Color) -> Self {
        let red = srgb_to_linear(color.r);
        let green = srgb_to_linear(color.g);
        let blue = srgb_to_linear(color.b);
        let long = 0.412_221_46_f32
            .mul_add(red, 0.536_332_55_f32.mul_add(green, 0.051_445_995 * blue))
            .cbrt();
        let medium = 0.211_903_5_f32
            .mul_add(red, 0.680_699_5_f32.mul_add(green, 0.107_396_96 * blue))
            .cbrt();
        let short = 0.088_302_46_f32
            .mul_add(red, 0.281_718_85_f32.mul_add(green, 0.629_978_7 * blue))
            .cbrt();
        Self {
            lightness: 0.210_454_26_f32.mul_add(
                long,
                0.793_617_8_f32.mul_add(medium, -0.004_072_047 * short),
            ),
            green_red: 1.977_998_5_f32.mul_add(
                long,
                (-2.428_592_2_f32).mul_add(medium, 0.450_593_7 * short),
            ),
            blue_yellow: 0.025_904_037_f32.mul_add(
                long,
                0.782_771_77_f32.mul_add(medium, -0.808_675_77 * short),
            ),
        }
    }

    #[allow(clippy::excessive_precision)]
    #[inline]
    fn into_srgb(self) -> Color {
        let long = 0.396_337_78_f32
            .mul_add(
                self.green_red,
                0.215_803_76_f32.mul_add(self.blue_yellow, self.lightness),
            )
            .powi(3);
        let medium = (-0.105_561_346_f32)
            .mul_add(
                self.green_red,
                (-0.063_854_17_f32).mul_add(self.blue_yellow, self.lightness),
            )
            .powi(3);
        let short = (-0.089_484_18_f32)
            .mul_add(
                self.green_red,
                (-1.291_485_5_f32).mul_add(self.blue_yellow, self.lightness),
            )
            .powi(3);
        Color::from_rgb(
            linear_to_srgb(4.076_741_7_f32.mul_add(
                long,
                (-3.307_711_6_f32).mul_add(medium, 0.230_969_94 * short),
            )),
            linear_to_srgb(
                (-1.268_438_f32)
                    .mul_add(long, 2.609_757_4_f32.mul_add(medium, -0.341_319_4 * short)),
            ),
            linear_to_srgb((-0.004_196_086_3_f32).mul_add(
                long,
                (-0.703_418_6_f32).mul_add(medium, 1.707_614_7 * short),
            )),
        )
    }

    #[inline]
    fn mix(self, other: Self, amount: f32) -> Self {
        let interpolate = |from: f32, to: f32| (to - from).mul_add(amount, from);
        Self {
            lightness: interpolate(self.lightness, other.lightness),
            green_red: interpolate(self.green_red, other.green_red),
            blue_yellow: interpolate(self.blue_yellow, other.blue_yellow),
        }
    }
}

#[inline]
fn srgb_to_linear(channel: f32) -> f32 {
    if channel <= 0.040_45 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
}

#[inline]
fn linear_to_srgb(channel: f32) -> f32 {
    let encoded = if channel <= 0.003_130_8 {
        12.92 * channel
    } else {
        1.055 * channel.powf(1.0 / 2.4) - 0.055
    };
    encoded.clamp(0.0, 1.0)
}

impl Default for TerminalPalette {
    fn default() -> Self {
        let rgb = Color::from_rgb8;
        Self {
            ansi: [
                rgb(0, 0, 0),
                rgb(170, 0, 0),
                rgb(0, 170, 0),
                rgb(170, 170, 0),
                rgb(0, 0, 170),
                rgb(170, 0, 170),
                rgb(0, 170, 170),
                rgb(204, 204, 204),
                rgb(85, 85, 85),
                rgb(255, 85, 85),
                rgb(85, 255, 85),
                rgb(255, 255, 85),
                rgb(85, 85, 255),
                rgb(255, 85, 255),
                rgb(85, 255, 255),
                rgb(255, 255, 255),
            ],
            foreground: rgb(204, 204, 204),
            background: rgb(15, 15, 14),
            echo: rgb(192, 255, 255),
            warn: rgb(255, 192, 85),
            output: rgb(255, 255, 192),
            selection: rgb(60, 60, 60),
            input_background: rgb(7, 7, 6),
        }
    }
}

#[derive(Clone)]
pub struct TerminalPrefs {
    pub font: Font,
    pub font_size: f32,
    pub ligatures: bool,
    pub bold_mode: TerminalBoldMode,
    pub disable_blink: bool,
    pub line_height: f32,
    pub line_length: Option<u16>,
    pub link_tooltip_delay_ms: u64,
    pub palette: Arc<TerminalPalette>,
    pub theme_extended_colors: bool,
    pub command_input_behavior: CommandInputBehavior,
    pub mask_input_on_server_echo: bool,
    pub history_case_sensitive_match: bool,
    pub max_history: usize,
    pub hide_pane_headers: bool,
    pub generation: u64,
}

impl Default for TerminalPrefs {
    fn default() -> Self {
        Self {
            font: GEIST_MONO,
            font_size: 15.0,
            ligatures: false,
            bold_mode: TerminalBoldMode::default(),
            disable_blink: false,
            line_height: 19.0,
            line_length: None,
            link_tooltip_delay_ms: 0,
            palette: Arc::new(TerminalPalette::default()),
            theme_extended_colors: false,
            command_input_behavior: CommandInputBehavior::default(),
            mask_input_on_server_echo: true,
            history_case_sensitive_match: false,
            max_history: 1_000,
            hide_pane_headers: false,
            generation: 0,
        }
    }
}

impl TerminalPrefs {
    #[must_use]
    #[inline]
    pub fn resolve(&self, color: VtColor) -> Color {
        self.palette.resolve(color, self.theme_extended_colors)
    }
}

static PREFS: LazyLock<ArcSwap<TerminalPrefs>> =
    LazyLock::new(|| ArcSwap::from_pointee(TerminalPrefs::default()));

#[must_use]
pub fn current() -> Arc<TerminalPrefs> {
    PREFS.load_full()
}

pub fn set_current(prefs: TerminalPrefs) {
    PREFS.store(Arc::new(prefs));
}

#[cfg(test)]
pub(crate) fn lock_prefs_test() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

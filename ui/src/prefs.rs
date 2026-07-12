//! App-global, hot-swappable terminal/appearance preferences.
//!
//! A single [`TerminalPrefs`] lives behind an `ArcSwap`; view/layout code
//! loads it per frame (cheap, lock-free) so settings changes apply live.
//! [`apply`] swaps a new snapshot in (bumping `generation`, which paragraph
//! caches key on) — the daemon calls it after the settings window commits a
//! change, and once at startup.

use std::collections::HashSet;
use std::sync::{Arc, LazyLock, Mutex};

use arc_swap::ArcSwap;
use iced::{Background, Color, Font};
use smudgy_core::models::settings::{CommandInputBehavior, ScriptPalette, Settings, ThemeTweaks};
use smudgy_core::session::connection::vt_processor::AnsiColor;
use smudgy_core::session::styled_line::Color as VtColor;
use smudgy_cloud::parse_css_color;

use crate::assets;
use crate::components::color_picker::Hsv;

pub mod palettes;

/// A named color scheme: the terminal's ANSI palette, default
/// foreground/background, and the app chrome it implies.
#[derive(Clone, PartialEq)]
pub struct TerminalPalette {
    pub name: &'static str,
    /// Indexed `[normal 8, bright 8]` in ANSI order (black, red, green,
    /// yellow, blue, magenta, cyan, white).
    pub ansi: [Color; 16],
    /// Default text color (plain server text, SGR 0/39). Distinct from
    /// `ansi[7]` so light schemes stay readable.
    pub foreground: Color,
    /// The terminal/window background; also the zero point of the
    /// archetypal RGB mapping.
    pub background: Color,
    pub echo: Color,
    pub warn: Color,
    pub output: Color,
    pub selection: Color,
    /// Background of the command-input strip. Its contrast against
    /// `background` (the terminal behind it) is a deliberate design element —
    /// every scheme picks this pairing, usually the scheme's companion
    /// surface color (darker for dark themes, dimmer for light ones).
    pub input_background: Color,
    /// Accent for the app theme; `None` falls back to the foreground.
    pub accent: Option<Color>,
    /// When true, the app chrome (backgrounds, text, modals…) is derived
    /// from this palette; false keeps the stock smudgy theme untouched.
    pub derive_app_theme: bool,
}

impl TerminalPalette {
    /// Maps a styled-line color through this palette.
    #[must_use]
    pub fn resolve(&self, vt_color: VtColor) -> Color {
        match vt_color {
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
            VtColor::Rgb { r, g, b } => self.archetypal(r, g, b),
            VtColor::Echo => self.echo,
            VtColor::Warn => self.warn,
            VtColor::Output => self.output,
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

    /// What `ESC[1m` on plain text renders as: bright white when the scheme
    /// gives it contrast, otherwise the plain foreground. Several canonical
    /// light schemes (Solarized Light, Tomorrow) define `ansi15` equal to
    /// their background — without this guard bolded default text would be
    /// invisible there.
    fn bright_default(&self) -> Color {
        let bright = self.ansi[15];
        let bg = self.background;
        let distance =
            (bright.r - bg.r).abs() + (bright.g - bg.g).abs() + (bright.b - bg.b).abs();
        if distance < 0.3 { self.foreground } else { bright }
    }

    /// Archetypal interpretation of a truecolor (and 256-color, which core
    /// flattens to RGB) value: each channel is an *intensity* interpolated
    /// between the theme background and the theme's bright primary for that
    /// channel, instead of between black and the pure sRGB primary. Servers
    /// that pick RGB colors assuming a black background therefore land on
    /// theme-coherent colors: (0,0,0) is the theme background, (255,0,0) is
    /// red as *this theme* says red, and grays ride the bg→white ramp.
    #[must_use]
    pub fn archetypal(&self, r: u8, g: u8, b: u8) -> Color {
        let bg = self.background;
        let bright_red = self.ansi[9];
        let bright_green = self.ansi[10];
        let bright_blue = self.ansi[12];
        let lerp = |from: f32, to: f32, t: f32| (to - from).mul_add(t, from);
        Color::from_rgb(
            lerp(bg.r, bright_red.r, f32::from(r) / 255.0),
            lerp(bg.g, bright_green.g, f32::from(g) / 255.0),
            lerp(bg.b, bright_blue.b, f32::from(b) / 255.0),
        )
    }
}

#[must_use]
pub fn palettes() -> &'static [&'static TerminalPalette] {
    &palettes::ALL
}

/// Looks a palette up by name, falling back to the default scheme.
#[must_use]
pub fn palette_by_name(name: &str) -> &'static TerminalPalette {
    palettes()
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case(name))
        .copied()
        .unwrap_or(&palettes::SMUDGY)
}

/// Font families bundled with the app (always available in the picker).
pub const BUNDLED_FONT_FAMILIES: &[&str] = &[
    "Geist Mono",
    "Monaspace Argon Var",
    "Monaspace Krypton Var",
    "Monaspace Neon Var",
    "Monaspace Radon Var",
    "Monaspace Xenon Var",
    "Courier Prime",
    "Departure Mono",
    "Fira Mono",
    "Lilex",
    "VT323",
];

/// The hot snapshot every terminal view reads per frame.
#[derive(Clone)]
pub struct TerminalPrefs {
    pub font: Font,
    pub font_size: f32,
    pub line_height: f32,
    /// Maximum line length in columns; `None` wraps at the pane width.
    pub line_length: Option<u16>,
    /// The effective palette: the chosen base scheme with the user's
    /// per-theme tweaks applied. Base schemes are never modified.
    pub palette: Arc<TerminalPalette>,
    /// What the command input does with the text after a send (and, for the
    /// default, on focus loss). Non-visual, so it never bumps `generation`.
    pub command_input_behavior: CommandInputBehavior,
    /// Hide pane headers unless the window's toolbar is expanded (the
    /// distraction-free rule; per-pane `always-show` overrides it). Read per
    /// frame by the pane-grid view; chrome-level, so it never bumps
    /// `generation`.
    pub hide_pane_headers: bool,
    /// Bumped on every [`apply`]; caches that bake prefs-derived data
    /// (paragraphs, span colors) key on it.
    pub generation: u64,
}

/// The effective terminal palette for `settings`: the chosen base scheme with the user's
/// per-theme tweaks applied (base schemes are never modified). Shared by [`TerminalPrefs`] and
/// the script-visible [`script_palette`] so both see identical colors.
#[must_use]
pub fn effective_palette(settings: &Settings) -> TerminalPalette {
    let base = palette_by_name(&settings.theme);
    settings
        .theme_tweaks
        .get(base.name)
        .filter(|tweaks| !tweaks.is_neutral())
        .map_or_else(|| base.clone(), |tweaks| apply_tweaks(base, tweaks))
}

/// The effective terminal palette as the script-visible [`ScriptPalette`] (each color a
/// `#rrggbb` hex string), for `smudgy:core`'s `getSettings().palette`. Resolved here because
/// color-scheme resolution lives in this (UI) crate, not in `smudgy_core`.
#[must_use]
pub fn script_palette(settings: &Settings) -> ScriptPalette {
    let palette = effective_palette(settings);
    let hex = crate::components::color_picker::to_hex;
    ScriptPalette {
        ansi: palette.ansi.iter().copied().map(hex).collect(),
        foreground: hex(palette.foreground),
        background: hex(palette.background),
        echo: hex(palette.echo),
        warn: hex(palette.warn),
        output: hex(palette.output),
        selection: hex(palette.selection),
        input_background: hex(palette.input_background),
        accent: palette.accent.map(hex),
    }
}

impl TerminalPrefs {
    fn from_settings(settings: &Settings, generation: u64) -> Self {
        let font_size = settings.terminal_font_size.clamp(8.0, 40.0);
        Self {
            font: font_for_family(&settings.terminal_font_family),
            font_size,
            line_height: (font_size * 1.25).round(),
            // Clamp here too: hand-edited settings.json bypasses the UI
            // validation, and 0 columns would shape zero-width paragraphs.
            line_length: settings.terminal_line_length.map(|len| len.clamp(20, 1000)),
            palette: Arc::new(effective_palette(settings)),
            command_input_behavior: settings.command_input_behavior,
            hide_pane_headers: settings.hide_pane_headers,
            generation,
        }
    }
}

/// How far the background slider can move surface lightness at ±1.
const SURFACE_RANGE: f32 = 0.35;
/// How far the brightness slider can move text lightness at ±1.
const TEXT_RANGE: f32 = 0.25;

fn shift_lightness(color: Color, offset: f32) -> Color {
    let mut hsv = Hsv::from_color(color);
    hsv.value = (hsv.value + offset).clamp(0.0, 1.0);
    hsv.to_color()
}

fn scale_saturation(color: Color, t: f32) -> Color {
    let mut hsv = Hsv::from_color(color);
    hsv.saturation = (hsv.saturation * (1.0 + t)).clamp(0.0, 1.0);
    hsv.to_color()
}

/// Expands (t > 0) or compresses (t < 0) a color's per-channel distance from
/// `anchor` — the same background-anchored notion of contrast the archetypal
/// RGB mapping uses.
fn expand_from(color: Color, anchor: Color, t: f32) -> Color {
    let factor = 1.0 + t;
    let stretch =
        |c: f32, a: f32| (c - a).mul_add(factor, a).clamp(0.0, 1.0);
    Color {
        r: stretch(color.r, anchor.r),
        g: stretch(color.g, anchor.g),
        b: stretch(color.b, anchor.b),
        a: color.a,
    }
}

/// The override slot names, in display order: surfaces and roles first, then
/// the 16 ANSI slots (`ansi0`..`ansi15`).
#[must_use]
pub fn override_slots() -> Vec<&'static str> {
    let mut slots = vec![
        "background",
        "foreground",
        "input_background",
        "selection",
        "echo",
        "warn",
        "output",
    ];
    slots.extend(ANSI_SLOT_NAMES);
    slots
}

const ANSI_SLOT_NAMES: [&str; 16] = [
    "ansi0", "ansi1", "ansi2", "ansi3", "ansi4", "ansi5", "ansi6", "ansi7", "ansi8", "ansi9",
    "ansi10", "ansi11", "ansi12", "ansi13", "ansi14", "ansi15",
];

/// Reads a slot's current color from a palette.
#[must_use]
pub fn slot_color(palette: &TerminalPalette, slot: &str) -> Option<Color> {
    if let Some(index) = ANSI_SLOT_NAMES.iter().position(|name| *name == slot) {
        return Some(palette.ansi[index]);
    }
    match slot {
        "background" => Some(palette.background),
        "foreground" => Some(palette.foreground),
        "input_background" => Some(palette.input_background),
        "selection" => Some(palette.selection),
        "echo" => Some(palette.echo),
        "warn" => Some(palette.warn),
        "output" => Some(palette.output),
        _ => None,
    }
}

fn set_slot_color(palette: &mut TerminalPalette, slot: &str, color: Color) {
    if let Some(index) = ANSI_SLOT_NAMES.iter().position(|name| *name == slot) {
        palette.ansi[index] = color;
        return;
    }
    match slot {
        "background" => palette.background = color,
        "foreground" => palette.foreground = color,
        "input_background" => palette.input_background = color,
        "selection" => palette.selection = color,
        "echo" => palette.echo = color,
        "warn" => palette.warn = color,
        "output" => palette.output = color,
        _ => {}
    }
}

/// Applies non-destructive tweaks to a base scheme: surface lightness, text
/// brightness/saturation, background-anchored contrast, then verbatim
/// per-slot overrides. Also used by the Preferences panel to render live
/// previews while sliders move.
#[must_use]
pub fn apply_tweaks(base: &TerminalPalette, tweaks: &ThemeTweaks) -> TerminalPalette {
    let mut palette = base.clone();

    // Surfaces move together; text contrast is preserved (and then anchored
    // on the *moved* background below).
    let surface_offset = tweaks.background.clamp(-1.0, 1.0) * SURFACE_RANGE;
    if surface_offset != 0.0 {
        palette.background = shift_lightness(palette.background, surface_offset);
        palette.input_background = shift_lightness(palette.input_background, surface_offset);
        palette.selection = shift_lightness(palette.selection, surface_offset);
    }

    let text_offset = tweaks.brightness.clamp(-1.0, 1.0) * TEXT_RANGE;
    let saturation = tweaks.saturation.clamp(-1.0, 1.0);
    let contrast = tweaks.contrast.clamp(-1.0, 1.0);
    let anchor = palette.background;

    let adjust_text = |color: Color| {
        let mut color = color;
        if text_offset != 0.0 {
            color = shift_lightness(color, text_offset);
        }
        if saturation != 0.0 {
            color = scale_saturation(color, saturation);
        }
        if contrast != 0.0 {
            color = expand_from(color, anchor, contrast);
        }
        color
    };

    for slot in &mut palette.ansi {
        *slot = adjust_text(*slot);
    }
    palette.foreground = adjust_text(palette.foreground);
    palette.echo = adjust_text(palette.echo);
    palette.warn = adjust_text(palette.warn);
    palette.output = adjust_text(palette.output);

    // Explicit overrides win exactly: what you picked is what you get.
    for (slot, hex) in &tweaks.overrides {
        if let Some(color) = parse_css_color(hex) {
            set_slot_color(&mut palette, slot, color);
        }
    }

    palette
}

static PREFS: LazyLock<ArcSwap<TerminalPrefs>> = LazyLock::new(|| {
    ArcSwap::from_pointee(TerminalPrefs::from_settings(&Settings::default(), 0))
});

/// The current preferences snapshot (lock-free).
#[must_use]
pub fn current() -> Arc<TerminalPrefs> {
    PREFS.load_full()
}

/// Swaps new settings in; takes effect on the next frame.
///
/// The cache generation only advances when a render-relevant field actually
/// changed — committing a non-visual setting (logging, separator…) must not
/// invalidate every paragraph cache and re-bake every session's scrollback.
pub fn apply(settings: &Settings) {
    let current = PREFS.load();
    let mut next = TerminalPrefs::from_settings(settings, current.generation);
    let visually_equal = next.font == current.font
        && next.font_size == current.font_size
        && next.line_height == current.line_height
        && next.line_length == current.line_length
        && *next.palette == *current.palette;
    if !visually_equal {
        next.generation = current.generation + 1;
    }
    publish_markdown_colors(&next.palette);
    PREFS.store(Arc::new(next));
}

/// Resolves the Markdown-widget colors for the effective palette and publishes
/// them to `smudgy_theme`. `smudgy_widgets` renders Markdown but can't reach the
/// terminal scheme (this crate depends on it, not the reverse), so the colors
/// are computed here and read back there. Body text tracks the terminal
/// foreground (so Markdown prose matches server text, not the brighter chrome);
/// links take the scheme accent, falling back to the scheme's cyan toned toward
/// the foreground for readability; code blocks stay a dark-grey panel on every
/// scheme, light ones included (the panel barely tracks the background so it
/// reads dark even on light schemes, where a mid-grey panel would wash out).
fn publish_markdown_colors(palette: &TerminalPalette) {
    let fg = palette.foreground;
    let bg = palette.background;
    // `ansi[6]` is the scheme's (normal) cyan; toning 20% toward the foreground keeps it readable
    // without the neon of the bright slot.
    let link = palette
        .accent
        .unwrap_or_else(|| mix(palette.ansi[6], fg, 0.2));
    smudgy_theme::markdown::set(smudgy_theme::markdown::MarkdownColors {
        body: fg,
        link,
        link_background: Color { a: 0.14, ..link },
        code_background: mix(Color::from_rgb8(22, 22, 24), bg, 0.10),
        code_foreground: mix(Color::from_rgb8(212, 212, 212), fg, 0.12),
    });
}

/// `Font::with_name` needs a `'static` family name; user-chosen names are
/// interned here. The leak is bounded by the set of distinct names chosen
/// in one app run.
fn intern_family(name: &str) -> &'static str {
    static INTERNED: LazyLock<Mutex<HashSet<&'static str>>> =
        LazyLock::new(|| Mutex::new(HashSet::new()));
    let mut set = INTERNED.lock().expect("font intern lock");
    if let Some(existing) = set.get(name) {
        existing
    } else {
        let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
        set.insert(leaked);
        leaked
    }
}

/// Resolves a configured family name to an iced `Font`, defaulting to the
/// bundled terminal font for empty names.
#[must_use]
pub fn font_for_family(family: &str) -> Font {
    let family = family.trim();
    if family.is_empty() {
        assets::fonts::GEIST_MONO_VF
    } else {
        Font::with_name(intern_family(family))
    }
}

/// Linear per-channel blend from `a` toward `b` (alpha kept from `a`).
fn mix(a: Color, b: Color, t: f32) -> Color {
    Color {
        r: (b.r - a.r).mul_add(t, a.r),
        g: (b.g - a.g).mul_add(t, a.g),
        b: (b.b - a.b).mul_add(t, a.b),
        a: a.a,
    }
}

/// The app theme for main windows. The stock scheme keeps the hand-tuned
/// smudgy chrome; every other palette derives readable chrome from its own
/// foreground/background pair (this is what makes light schemes usable —
/// stock text/modal colors assume a dark window).
#[must_use]
pub fn app_theme() -> smudgy_theme::Theme {
    let prefs = current();
    let palette = &prefs.palette;
    let mut theme = smudgy_theme::smudgy();
    if !palette.derive_app_theme {
        // Stock chrome — but the surfaces still follow the (possibly
        // tweaked) palette, since the terminal renders transparently over
        // the window background. Untweaked, these equal the stock values.
        theme.styles.general.background = palette.background;
        theme.styles.general.input_background = palette.input_background;
        return theme;
    }

    let bg = palette.background;
    let fg = palette.foreground;

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

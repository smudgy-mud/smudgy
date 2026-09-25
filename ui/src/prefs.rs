//! App-global, hot-swappable terminal/appearance preferences.
//!
//! A single [`TerminalPrefs`] lives behind an `ArcSwap`; view/layout code
//! loads it per frame (cheap, lock-free) so settings changes apply live.
//! [`apply`] swaps a new snapshot in (bumping `generation`, which paragraph
//! caches key on) — the daemon calls it after the settings window commits a
//! change, and once at startup.

use std::collections::HashSet;
use std::ops::Deref;
use std::sync::{Arc, LazyLock, Mutex};

use arc_swap::ArcSwap;
use iced::{Color, Font};
use smudgy_cloud::parse_css_color;
use smudgy_core::models::settings::{
    CommandInputBehavior, MAX_LINK_TOOLTIP_DELAY_MS, ScriptPalette, Settings, TerminalBoldMode,
    ThemeTweaks,
};
use smudgy_core::session::styled_line::Color as VtColor;
use smudgy_ui_shared::prefs as shared;

use crate::assets;
use crate::components::color_picker::Hsv;

pub use smudgy_ui_shared::palettes;
pub use smudgy_ui_shared::prefs::NamedTerminalPalette as TerminalPalette;

#[must_use]
pub fn palettes() -> &'static [&'static TerminalPalette] {
    shared::palettes()
}

/// Looks a palette up by name, falling back to the default scheme.
#[must_use]
pub fn palette_by_name(name: &str) -> &'static TerminalPalette {
    shared::palette_by_name(name)
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
    "Fixedsys Excelsior",
    "Lilex",
    "VT323",
];

/// Desktop snapshot. Shared render fields have one authoritative definition;
/// native-only settings retain their persisted enum and app-theme metadata.
#[derive(Clone)]
pub struct TerminalPrefs {
    render: shared::TerminalPrefs,
    /// Effective named palette, including desktop app-chrome metadata.
    pub palette: Arc<TerminalPalette>,
    pub bold_mode: TerminalBoldMode,
    pub command_input_behavior: CommandInputBehavior,
}

impl Deref for TerminalPrefs {
    type Target = shared::TerminalPrefs;

    fn deref(&self) -> &Self::Target {
        &self.render
    }
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
    /// Resolves one terminal color using the current extended-color policy.
    #[must_use]
    pub fn resolve(&self, color: VtColor) -> Color {
        self.palette.resolve(color, self.theme_extended_colors)
    }

    fn from_settings(settings: &Settings, generation: u64) -> Self {
        let font_size = settings.terminal_font_size.clamp(8.0, 40.0);
        let palette = Arc::new(effective_palette(settings));
        let bold_mode = settings.terminal_bold_mode;
        let command_input_behavior = settings.command_input_behavior;
        Self {
            render: shared::TerminalPrefs {
                font: font_for_family(&settings.terminal_font_family),
                font_size,
                ligatures: settings.terminal_font_ligatures,
                bold_mode,
                disable_blink: settings.terminal_disable_blink,
                line_height: (font_size * 1.25).round(),
                // Hand-edited settings.json bypasses the UI's validation.
                line_length: settings.terminal_line_length.map(|len| len.clamp(20, 1000)),
                link_tooltip_delay_ms: settings
                    .link_tooltip_delay_ms
                    .min(MAX_LINK_TOOLTIP_DELAY_MS),
                palette: Arc::new(palette.render.clone()),
                theme_extended_colors: settings.theme_extended_colors,
                command_input_behavior,
                mask_input_on_server_echo: settings.mask_input_on_server_echo,
                history_case_sensitive_match: settings.history_case_sensitive_match,
                max_history: settings.max_history,
                hide_pane_headers: settings.hide_pane_headers,
                generation,
            },
            palette,
            bold_mode: settings.terminal_bold_mode,
            command_input_behavior: settings.command_input_behavior,
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
    let stretch = |c: f32, a: f32| (c - a).mul_add(factor, a).clamp(0.0, 1.0);
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
    let prefs = TerminalPrefs::from_settings(&Settings::default(), 0);
    sync_terminal_ligatures(None, &prefs.font, prefs.ligatures);
    publish_shared_terminal_prefs(&prefs);
    ArcSwap::from_pointee(prefs)
});

/// By default the terminal family shapes without ligatures: MUD output assumes
/// a fixed character grid, and ligature/contextual substitutions (fi, `=>`,
/// Monaspace's texture healing) break column alignment. Registered per family
/// with the patched `iced_graphics` shaping registry
/// (patches/iced_graphics+0.14.0.patch), so UI families keep their ligatures;
/// a family that stops being the terminal font gets its ligatures back, and
/// the "Font ligatures" preference re-enables them in place.
fn sync_terminal_ligatures(previous: Option<&Font>, current: &Font, ligatures: bool) {
    use iced::font::Family;
    if let Some(Font {
        family: Family::Name(old),
        ..
    }) = previous
    {
        iced_graphics::text::set_ligatures_enabled(old, true);
    }
    if let Family::Name(name) = current.family {
        iced_graphics::text::set_ligatures_enabled(name, ligatures);
    }
}

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
    if next.font != current.font || next.ligatures != current.ligatures {
        sync_terminal_ligatures(Some(&current.font), &next.font, next.ligatures);
    }
    let visually_equal = next.font == current.font
        && next.ligatures == current.ligatures
        && next.bold_mode == current.bold_mode
        && next.font_size == current.font_size
        && next.line_height == current.line_height
        && next.line_length == current.line_length
        && *next.palette == *current.palette
        && next.theme_extended_colors == current.theme_extended_colors;
    if !visually_equal {
        next.render.generation = current.generation + 1;
    }
    publish_markdown_colors(&next.palette);
    publish_shared_terminal_prefs(&next);
    PREFS.store(Arc::new(next));
}

fn publish_shared_terminal_prefs(prefs: &TerminalPrefs) {
    shared::set_current(prefs.render.clone());
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
fn mix(a: Color, b: Color, amount: f32) -> Color {
    Color {
        r: (b.r - a.r).mul_add(amount, a.r),
        g: (b.g - a.g).mul_add(amount, a.g),
        b: (b.b - a.b).mul_add(amount, a.b),
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
    shared::app_theme_for_palette(&prefs.palette)
}

#[cfg(test)]
mod tests {
    use super::{Color, TerminalPalette, TerminalPrefs, palettes};
    use smudgy_core::models::settings::{MAX_LINK_TOOLTIP_DELAY_MS, Settings, TerminalBoldMode};

    fn assert_color_close(actual: Color, expected: Color) {
        const TOLERANCE: f32 = 0.000_1;
        assert!(
            (actual.r - expected.r).abs() < TOLERANCE
                && (actual.g - expected.g).abs() < TOLERANCE
                && (actual.b - expected.b).abs() < TOLERANCE,
            "actual {actual:?}, expected {expected:?}"
        );
    }

    #[test]
    fn archetypal_cube_hits_complete_palette_corners() {
        let palette = &palettes::SMUDGY;
        let cases = [
            ((0, 0, 0), palette.background),
            ((255, 0, 0), palette.ansi[9]),
            ((0, 255, 0), palette.ansi[10]),
            ((255, 255, 0), palette.ansi[11]),
            ((0, 0, 255), palette.ansi[12]),
            ((255, 0, 255), palette.ansi[13]),
            ((0, 255, 255), palette.ansi[14]),
            ((255, 255, 255), palette.ansi[15]),
        ];

        for ((red, green, blue), expected) in cases {
            assert_color_close(palette.archetypal(red, green, blue), expected);
        }
    }

    #[test]
    fn light_palette_cube_uses_full_colors_and_readable_white() {
        let palette: &TerminalPalette = &palettes::SOLARIZED_LIGHT;
        assert_color_close(palette.archetypal(255, 0, 0), palette.ansi[9]);
        // Solarized Light's bright-white slot equals its background, so the
        // same guard as bold default text selects the readable foreground.
        assert_color_close(palette.archetypal(255, 255, 255), palette.foreground);
    }

    #[test]
    fn literal_extended_colors_bypass_the_theme_cube() {
        use smudgy_core::session::styled_line::Color as VtColor;

        let palette = &palettes::SOLARIZED_LIGHT;
        let source = VtColor::Rgb {
            r: 95,
            g: 135,
            b: 175,
        };
        assert_eq!(
            palette.resolve(source, false),
            Color::from_rgb8(95, 135, 175)
        );
        assert_ne!(
            palette.resolve(source, true),
            Color::from_rgb8(95, 135, 175)
        );
    }

    #[test]
    fn every_named_palette_uses_the_shared_render_snapshot() {
        use smudgy_core::session::styled_line::Color as VtColor;

        for palette in palettes() {
            let settings = Settings {
                theme: palette.name.to_owned(),
                theme_extended_colors: true,
                ..Settings::default()
            };
            let prefs = TerminalPrefs::from_settings(&settings, 0);
            assert_eq!(prefs.palette.name, palette.name);
            assert!(prefs.render.palette.as_ref() == &prefs.palette.render);
            for color in [
                VtColor::Echo,
                VtColor::Warn,
                VtColor::Output,
                VtColor::DefaultForeground { bold: true },
                VtColor::Rgb {
                    r: 95,
                    g: 135,
                    b: 175,
                },
            ] {
                assert_eq!(prefs.resolve(color.clone()), prefs.render.resolve(color));
            }
        }
    }

    #[test]
    fn native_initialization_publishes_defaults_to_shared_widgets() {
        let native = super::current();
        let shared = smudgy_ui_shared::prefs::current();
        assert!(shared.theme_extended_colors);
        assert_eq!(native.theme_extended_colors, shared.theme_extended_colors);
        assert!(native.palette.render == *shared.palette);
        assert_eq!(native.generation, shared.generation);
    }

    #[test]
    fn tooltip_delay_defaults_immediately_and_clamps_hand_edits() {
        assert_eq!(
            TerminalPrefs::from_settings(&Settings::default(), 0).link_tooltip_delay_ms,
            0
        );
        let settings = Settings {
            link_tooltip_delay_ms: u64::MAX,
            ..Settings::default()
        };
        assert_eq!(
            TerminalPrefs::from_settings(&settings, 0).link_tooltip_delay_ms,
            MAX_LINK_TOOLTIP_DELAY_MS
        );
    }

    #[test]
    fn terminal_bold_mode_follows_settings_and_defaults_to_both() {
        assert_eq!(
            TerminalPrefs::from_settings(&Settings::default(), 0).bold_mode,
            TerminalBoldMode::BoldAndBright
        );
        for mode in TerminalBoldMode::ALL {
            let settings = Settings {
                terminal_bold_mode: mode,
                ..Settings::default()
            };
            assert_eq!(TerminalPrefs::from_settings(&settings, 0).bold_mode, mode);
        }
    }

    #[test]
    fn disable_blink_defaults_off_and_follows_settings() {
        assert!(!TerminalPrefs::from_settings(&Settings::default(), 0).disable_blink);
        let settings = Settings {
            terminal_disable_blink: true,
            ..Settings::default()
        };
        assert!(TerminalPrefs::from_settings(&settings, 0).disable_blink);
    }
}

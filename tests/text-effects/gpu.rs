//! Hardware evidence for the package catalogue and shared playback ABI.
use super::*;
use iced::{
    Font, Pixels, Point, Size,
    advanced::{
        Renderer as _,
        renderer::Headless,
        text::{self as api, Paragraph as _, Renderer as _},
    },
};
use smudgy_session_model::{
    inline_content::{InlineOwner, TextEffect},
    text_shader::{EffectScale, Shader, ShaderEffect},
};
use std::time::Duration;
struct RestorePrefs(crate::prefs::TerminalPrefs);
impl Drop for RestorePrefs {
    fn drop(&mut self) {
        crate::prefs::set_current(self.0.clone());
    }
}
#[path = "fixtures.rs"]
mod fixtures;
use fixtures::*;

fn difference(a: &[u8], b: &[u8]) -> u64 {
    a.iter()
        .zip(b)
        .map(|(a, b)| u64::from(a.abs_diff(*b)))
        .sum()
}
#[test]
#[ignore = "requires hardware GPU; run serialized"]
fn catalogue_scale_fades_and_pane_overflow_preserve_text_and_reuse_glyphs() {
    check_catalogue(false, false);
}
#[test]
#[ignore = "requires hardware GPU; run serialized"]
fn watching_counters_do_not_repaint_a_neighboring_plain_row() {
    check_catalogue(true, false);
}
#[test]
#[ignore = "requires hardware GPU; run serialized"]
fn materials_remain_readable_on_light_backgrounds() {
    check_catalogue(false, true);
}
// Share fixed-input capture/playback checks across the catalogue regressions.
#[allow(clippy::too_many_lines)]
fn check_catalogue(row_regression: bool, light: bool) {
    let _prefs_lock = crate::prefs::lock_prefs_test();
    let _restore = RestorePrefs((*crate::prefs::current()).clone());
    crate::prefs::set_current(crate::prefs::TerminalPrefs::default());
    let foreground = if light {
        iced::Color::from_rgb(0.08, 0.09, 0.11)
    } else {
        iced::Color::WHITE
    };
    if light {
        let mut prefs = (*crate::prefs::current()).clone();
        let palette = Arc::make_mut(&mut prefs.palette);
        palette.background = iced::Color::from_rgb(0.96, 0.95, 0.92);
        palette.foreground = foreground;
        crate::prefs::set_current(prefs);
    }
    let font = Font::MONOSPACE;
    let mut renderer = iced::futures::executor::block_on(<iced_wgpu::Renderer as Headless>::new(
        Font::MONOSPACE,
        Pixels(16.0),
        Some("wgpu"),
    ))
    .unwrap();
    register_renderer(&renderer);
    let caption: String = if row_regression {
        "oooooooooooo\nllllllllllll".into()
    } else {
        "Aria steps into view.".into()
    };
    let spans = [api::Span::<()>::new(&caption).color(foreground)];
    let paragraph = text::Paragraph::with_spans(api::Text {
        content: &spans[..],
        bounds: Size::new(650.0, 60.0),
        size: Pixels(28.0),
        line_height: api::LineHeight::default(),
        font,
        align_x: api::Alignment::Left,
        align_y: iced::alignment::Vertical::Top,
        shaping: api::Shaping::Advanced,
        wrapping: api::Wrapping::None,
    });
    let input =
        Input::from_paragraph(&paragraph, Rectangle::with_size(paragraph.min_bounds())).unwrap();
    let screen = Rectangle::with_size(Size::new(768.0, 432.0));
    let clip = Rectangle::new(Point::new(24.0, 24.0), Size::new(720.0, 384.0));
    let at = Point::new(216.0, 208.0);
    let bounds = Rectangle::new(at, paragraph.min_bounds());
    let background = crate::prefs::current().palette.background;
    renderer.reset(screen);
    let blank = Headless::screenshot(&mut renderer, Size::new(768, 432), 1.0, background);
    let mut capture = |effect: Option<&InlineDecoration>, seconds: f32| {
        renderer.reset(screen);

        let mut hide = false;
        if let Some(effect) = effect {
            let now = effect.started + std::time::Duration::from_secs_f32(seconds);
            let admitted = draw(&mut renderer, &input, bounds, clip, effect, now);
            hide = admitted && effect.effect.shader.replace;
        }
        if !hide {
            renderer.with_layer(clip, |r| r.fill_paragraph(&paragraph, at, foreground, clip));
        }

        Headless::screenshot(&mut renderer, Size::new(768, 432), 1.0, background)
    };
    let baseline = capture(None, 0.0);
    // Supersampled font outlines are re-hinted and differ from the native atlas.
    // Bound that error relative to the actual ink, independent of caption length.
    let rest_tolerance = (difference(&baseline, &blank) / 5).max(100_000);
    let catalogue = cases();
    let mut original_index = 0_u64;
    for case in catalogue {
        let mut effect = case.decoration();
        let shader = effect.effect.shader.shader.clone();
        prewarm(&shader).unwrap();
        // Keep procedural seeds reproducible when other tests allocate inline instances first.
        // A separate high ID range also avoids collisions with those ordinary instances.
        let seed_index = if kinetic_signal(case.name)
            || anchored_intro(case.name)
            || matches!(
                case.name,
                "implode"
                    | "wind"
                    | "sand"
                    | "recoil"
                    | "accordion"
                    | "pendulum"
                    | "domino"
                    | "tumble"
                    | "magnetic"
                    | "vortex"
                    | "shatter"
                    | "unravel"
                    | "liquefy"
                    | "inkBloom"
                    | "duel"
                    | "eclipse"
                    | "dimensionalRift"
                    | "shockfront"
                    | "judgement"
                    | "ascension"
                    | "reverseSmoke"
            ) {
            256 + case
                .name
                .bytes()
                .fold(0_u64, |hash, byte| (hash * 31 + u64::from(byte)) % 65536)
        } else {
            let index = original_index;
            original_index += 1;
            index
        };
        effect.id = (1_u64 << 32) + seed_index + 1;
        if light && !matches!(case.name, "crystal" | "lava" | "decay") {
            continue;
        }
        if row_regression && case.name != "watchers" {
            continue;
        }
        let start = capture(Some(&effect), 0.0);
        // GPU replacement glyphs differ slightly from the native atlas, so compare the end exactly and start with tolerance.
        assert!(
            matches!(
                case.name,
                "implode"
                    | "reverseSmoke"
                    | "accordion"
                    | "tumble"
                    | "magnetic"
                    | "vortex"
                    | "inkBloom"
                    | "duel"
                    | "dimensionalRift"
                    | "shockfront"
                    | "judgement"
            ) || difference(&start, &baseline) < rest_tolerance,
            "{} starts with ordinary text: difference={}, tolerance={rest_tolerance}",
            case.name,
            difference(&start, &baseline)
        );
        let time = match case.name {
            "radiate" => 0.10,
            "lightning" => 0.40,
            "critical" => 0.48,
            "blessing" => 1.45,
            _ => 0.85,
        };
        let warmed_uploads = input.stats().uploads;
        let first = capture(Some(&effect), time);
        if row_regression {
            let mut quiet = effect.clone();
            let settings = &mut quiet.effect.shader;
            let mut params = case.params.clone();
            params["intensity"] = 0.0.into();
            Arc::make_mut(settings).uniforms =
                shader.uniforms(params.as_object().unwrap()).unwrap();
            let ordinary = capture(Some(&quiet), time);
            let changed = first
                .chunks_exact(4)
                .zip(ordinary.chunks_exact(4))
                .enumerate()
                .filter(|(i, (watching, plain))| {
                    let p = Point::new((i % 768) as f32, (i / 768) as f32);
                    bounds.contains(p)
                        && p.y > bounds.center_y()
                        && difference(&watching[..3], &plain[..3]) > 10
                })
                .count();
            assert!(
                changed < 8,
                "watching counters must leave the plain row alone: {changed}"
            );
        }
        let visible = difference(&first, &baseline);
        if kinetic_signal(case.name) {
            // These are attention signals, never text replacements. Check the
            // source's opaque ink throughout both impacts, not only at rest.
            let core_contrast = baseline
                .chunks_exact(4)
                .zip(blank.chunks_exact(4))
                .enumerate()
                .filter(|(i, _)| bounds.contains(Point::new((i % 768) as f32, (i / 768) as f32)))
                .map(|(_, (ink, bg))| difference(&ink[..3], &bg[..3]))
                .max()
                .unwrap();
            for seconds in [0.08, 0.25, 0.55, 0.85, 1.4, 1.9] {
                let frame = capture(Some(&effect), seconds);
                let mut checked = 0;
                for (i, ((ink, native), bg)) in frame
                    .chunks_exact(4)
                    .zip(baseline.chunks_exact(4))
                    .zip(blank.chunks_exact(4))
                    .enumerate()
                {
                    if bounds.contains(Point::new((i % 768) as f32, (i / 768) as f32))
                        && difference(&native[..3], &bg[..3]) * 100 >= core_contrast * 98
                    {
                        assert!(
                            difference(&ink[..3], &native[..3]) < 30,
                            "{} preserves readable native ink at {seconds}s: pixel {i}",
                            case.name
                        );
                        checked += 1;
                    }
                }
                assert!(checked > 30, "enough native ink to measure readability");
            }
            let mut repeating = effect.clone();
            repeating.effect.duration_ms = 0;
            assert!(
                difference(
                    &capture(Some(&repeating), 0.85),
                    &capture(Some(&repeating), 3.25)
                ) < 2_000,
                "{} repeats without positional drift",
                case.name
            );
        }
        if light && matches!(case.name, "crystal" | "lava" | "decay") {
            let material = capture(Some(&effect), 1.5);
            let mut native_contrast = 0_u64;
            let mut material_contrast = 0_u64;
            for (i, ((ink, original), background)) in material
                .chunks_exact(4)
                .zip(baseline.chunks_exact(4))
                .zip(blank.chunks_exact(4))
                .enumerate()
            {
                if bounds.contains(Point::new((i % 768) as f32, (i / 768) as f32)) {
                    let native = difference(&original[..3], &background[..3]);
                    if native > 180 {
                        native_contrast += native;
                        material_contrast += difference(&ink[..3], &background[..3]);
                    }
                }
            }
            assert!(
                material_contrast * 4 > native_contrast,
                "{} material remains readable on light backgrounds: {material_contrast}/{native_contrast}",
                case.name
            );
        }
        assert!(
            visible > 20_000,
            "{} must be visibly active: {visible}",
            case.name
        );
        if case.name == "lightning" {
            // Both flashes stay around the strike even when authors opt into pane overflow.
            for pane in [false, true] {
                let mut local = effect.clone();
                let settings = &mut local.effect.shader;
                Arc::make_mut(settings).pane = pane;
                for seconds in [0.612, 0.731] {
                    let flash = capture(Some(&local), seconds);
                    assert!(difference(&flash, &baseline) > 2_000);
                    for (x, y) in [(44, 40), (710, 350), (80, 208)] {
                        let i = (y * 768 + x) * 4;
                        assert_eq!(
                            &flash[i..i + 4],
                            &baseline[i..i + 4],
                            "lightning flash stays local (pane={pane}, seconds={seconds})"
                        );
                    }
                }
            }
        }
        let late = capture(Some(&effect), 2.395);
        assert!(
            difference(&late, &baseline) < visible,
            "{} fades before removal",
            case.name
        );
        let settings = &mut effect.effect.shader;
        Arc::make_mut(settings).scale = EffectScale::new(2.0).unwrap();
        effect.effect.outset = case.outset * 2;
        let scaled = capture(Some(&effect), time);
        assert!(
            difference(&first, &scaled) > 10_000,
            "{} honors scale",
            case.name
        );
        for (x, y) in [(0, 0), (12, 200), (755, 200), (350, 420)] {
            let i = (y * 768 + x) * 4;
            assert_eq!(
                &scaled[i..i + 4],
                &baseline[i..i + 4],
                "{} respects pane clip",
                case.name
            );
        }
        assert_eq!(
            input.stats().uploads,
            warmed_uploads,
            "{} active time/scale changes reuse glyphs",
            case.name
        );
        if attention_burst(case.name) {
            let mut looping = effect.clone();
            looping.effect.duration_ms = 0;
            let first_cycle = capture(Some(&looping), 0.8);
            assert!(difference(&first_cycle, &baseline) > visible);
            assert!(
                difference(&first_cycle, &capture(Some(&looping), 3.2)) < 2_000,
                "{} repeats its zero-duration burst without drifting",
                case.name
            );
            let mut transparent = effect.clone();
            let settings = &mut transparent.effect.shader;
            let mut params = case.params.clone();
            for colour in ["base", "bright", "accent"] {
                params[colour][3] = 0.0.into();
            }
            Arc::make_mut(settings).uniforms =
                shader.uniforms(params.as_object().unwrap()).unwrap();
            assert!(
                difference(&capture(Some(&transparent), time), &baseline) < rest_tolerance,
                "{} transparent colours preserve ordinary text",
                case.name
            );
        }
        if attention_burst(case.name)
            || anchored_intro(case.name)
            || shader.fragments_per_glyph > 0
            || case.params.get("amplitude").is_some()
        {
            for key in ["intensity", "speed"] {
                let mut quiet = effect.clone();
                let settings = &mut quiet.effect.shader;
                let mut params = case.params.clone();
                params[key] = 0.0.into();
                Arc::make_mut(settings).uniforms =
                    shader.uniforms(params.as_object().unwrap()).unwrap();
                assert!(
                    difference(&capture(Some(&quiet), time), &baseline) < rest_tolerance,
                    "{} {key}=0 preserves ordinary text",
                    case.name
                );
            }
            for (key, values) in if matches!(case.name, "explode" | "implode") {
                vec![
                    ("pieces", [2.0, 4.0]),
                    ("depth", [0.0, 2.0]),
                    ("spin", [0.0, 2.0]),
                    ("spread", [0.5, 2.0]),
                ]
            } else if attention_burst(case.name)
                || material_batch(case.name)
                || matches!(
                    case.name,
                    "silence"
                        | "possession"
                        | "trueSight"
                        | "forgetting"
                        | "soulDrain"
                        | "voidBloom"
                        | "watchers"
                        | "mycelium"
                        | "barkMend"
                        | "ritualKnot"
                        | "counterSeal"
                )
            {
                let mut controls = vec![("speed", [0.5, 2.0])];
                if transformation_batch(case.name) {
                    controls.push(("intensity", [1.0, 2.0]));
                }
                if case.params.get("amplitude").is_some() {
                    controls.push(("amplitude", [0.0, 2.0]));
                }
                controls
            } else if case.params.get("amplitude").is_some() {
                vec![("amplitude", [0.0, 2.0])]
            } else {
                vec![]
            } {
                let mut variants = Vec::new();
                for value in values {
                    let mut variant = effect.clone();
                    let settings = &mut variant.effect.shader;
                    let mut params = case.params.clone();
                    params[key] = value.into();
                    Arc::make_mut(settings).uniforms =
                        shader.uniforms(params.as_object().unwrap()).unwrap();
                    if transformation_batch(case.name) {
                        // Check each control at its public default artwork scale;
                        // bounded pressure and aperture size can saturate at larger scales.
                        Arc::make_mut(settings).scale = EffectScale::default();
                    }
                    variants.push(capture(Some(&variant), time));
                }
                assert!(
                    difference(&variants[0], &variants[1]) > 1_000,
                    "{} honors {key}",
                    case.name
                );
            }
            assert_eq!(
                input.stats().uploads,
                warmed_uploads,
                "{} control changes do not recapture",
                case.name
            );
        }
        if case.params.get("mode").is_some()
            && !matches!(case.name, "recoil" | "pendulum" | "domino")
        {
            let mut images = Vec::new();
            for mode in [0.0, 1.0, 2.0] {
                let mut variant = effect.clone();
                let settings = &mut variant.effect.shader;
                let settings = Arc::make_mut(settings);
                settings.scale = EffectScale::default();
                settings.hold = true;
                let mut params = case.params.clone();
                params["mode"] = mode.into();
                settings.uniforms = shader.uniforms(params.as_object().unwrap()).unwrap();
                images.push(capture(Some(&variant), 0.70));
                let held = capture(Some(&variant), 2.4);
                if mode == 1.0 {
                    assert!(
                        difference(&held, &baseline) > 2_000,
                        "{} departure+hold keeps its transformed final image",
                        case.name
                    );
                } else {
                    assert!(
                        difference(&held, &baseline) < rest_tolerance,
                        "{} arrival/cycle+hold is readable",
                        case.name
                    );
                }
            }
            assert!(
                difference(&images[0], &images[1]) > 2_000,
                "{} distinguishes arrival from departure",
                case.name
            );
            assert!(
                difference(&images[1], &images[2]) > 2_000,
                "{} distinguishes departure from cycle",
                case.name
            );
            assert_eq!(
                input.stats().uploads,
                warmed_uploads,
                "mode changes reuse glyphs"
            );
        }
        if case.name == "wind" {
            let mut left = effect.clone();
            let settings = &mut left.effect.shader;
            let mut params = case.params.clone();
            params["direction"] = (-1.0).into();
            Arc::make_mut(settings).uniforms =
                shader.uniforms(params.as_object().unwrap()).unwrap();
            assert!(
                difference(&capture(Some(&left), 0.32), &capture(Some(&effect), 0.32)) > 2_000,
                "wind gust direction changes its trajectory"
            );
        }
        if case.name == "judgement" {
            let frame = capture(Some(&effect), 1.55);
            let corner = (40 * 768 + 44) * 4;
            assert_eq!(
                &frame[corner..corner + 4],
                &baseline[corner..corner + 4],
                "judgement beam does not flood a remote pane corner"
            );
        }
        if case.name == "thorns" {
            let mut vines = effect.clone();
            let settings = &mut vines.effect.shader;
            let settings = Arc::make_mut(settings);
            settings.scale = EffectScale::default();
            settings.hold = true;
            settings.fade_out_ms = 0;
            let early = capture(Some(&vines), 0.25);
            let grown = capture(Some(&vines), 2.4);
            assert!(difference(&grown, &baseline) > difference(&early, &baseline) * 2);
            assert_eq!(
                grown,
                capture(Some(&vines), 6.0),
                "grown thorns stop moving"
            );
            let mut quiet = vines.clone();
            let settings = &mut quiet.effect.shader;
            let mut params = case.params.clone();
            params["intensity"] = 0.0.into();
            Arc::make_mut(settings).uniforms =
                shader.uniforms(params.as_object().unwrap()).unwrap();
            let plain = capture(Some(&quiet), 2.4);
            let crossing_ink = grown
                .chunks_exact(4)
                .zip(plain.chunks_exact(4))
                .filter(|(plant, ink)| {
                    let source_contrast = ink[0].abs_diff(blank[0]);
                    source_contrast > 180 && difference(&plant[..3], &ink[..3]) > 24
                })
                .count();
            assert!(
                crossing_ink > 8,
                "vines visibly pass in front of glyph ink: {crossing_ink}"
            );
            for key in ["intensity", "speed"] {
                let mut quiet = vines.clone();
                let settings = &mut quiet.effect.shader;
                let mut params = case.params.clone();
                params[key] = 0.0.into();
                Arc::make_mut(settings).uniforms =
                    shader.uniforms(params.as_object().unwrap()).unwrap();
                assert!(difference(&capture(Some(&quiet), 1.0), &baseline) < rest_tolerance);
            }
            assert_eq!(
                input.stats().uploads,
                warmed_uploads,
                "growing and held vines reuse captured ink"
            );
        }
        if matches!(case.name, "silence" | "forgetting") {
            let mut gesture = effect.clone();
            gesture.effect.outset = case.outset;
            let settings = &mut gesture.effect.shader;
            Arc::make_mut(settings).scale = EffectScale::default();
            let mut quiet = gesture.clone();
            let settings = &mut quiet.effect.shader;
            let mut params = case.params.clone();
            params["intensity"] = 0.0.into();
            Arc::make_mut(settings).uniforms =
                shader.uniforms(params.as_object().unwrap()).unwrap();
            // Compare the same cached capture, so native small-font hinting is not counted as lost ink.
            let ordinary = capture(Some(&quiet), 1.20);
            let depleted = capture(Some(&gesture), 1.20);
            let mut ink = 0;
            let mut absent = 0;
            let mut remaining = 0;
            for (i, (pixel, original)) in depleted
                .chunks_exact(4)
                .zip(ordinary.chunks_exact(4))
                .enumerate()
            {
                let native = difference(&original[..3], &blank[..3]);
                if bounds.contains(Point::new((i % 768) as f32, (i / 768) as f32)) && native > 300 {
                    ink += 1;
                    let substance = difference(&pixel[..3], &blank[..3]);
                    absent += usize::from(substance * 3 < native);
                    remaining += usize::from(substance * 8 > native);
                }
            }
            assert!(ink > 20);
            assert!(
                absent * 4 > ink,
                "{} loses source substance: {absent}/{ink}",
                case.name
            );
            assert!(remaining * 8 > ink, "{} keeps a last impression", case.name);
        }
        if case.name == "trueSight" {
            let mut sight = effect.clone();
            sight.effect.outset = case.outset;
            let settings = &mut sight.effect.shader;
            Arc::make_mut(settings).scale = EffectScale::default();
            let mut quiet = sight.clone();
            let settings = &mut quiet.effect.shader;
            let mut params = case.params.clone();
            params["intensity"] = 0.0.into();
            Arc::make_mut(settings).uniforms =
                shader.uniforms(params.as_object().unwrap()).unwrap();
            let ordinary = capture(Some(&quiet), 1.14);
            let revealing = capture(Some(&sight), 1.14);
            let error = |left: bool| -> (u64, u64) {
                let mut deviation = 0;
                let mut energy = 0;
                for (i, (seen, source)) in revealing
                    .chunks_exact(4)
                    .zip(ordinary.chunks_exact(4))
                    .enumerate()
                {
                    let p = Point::new((i % 768) as f32, (i / 768) as f32);
                    let x = (p.x - bounds.x) / bounds.width;
                    let native = difference(&source[..3], &blank[..3]);
                    if bounds.contains(p) && native > 100 && if left { x < 0.25 } else { x > 0.75 }
                    {
                        energy += native;
                        deviation += difference(&seen[..3], &source[..3]);
                    }
                }
                (deviation, energy.max(1))
            };
            let (left_error, left_ink) = error(true);
            let (right_error, right_ink) = error(false);
            assert!(
                left_error * right_ink * 2 < right_error * left_ink,
                "true sight recovers the reading behind its sweep before clearing the far end"
            );
        }
        if case.name == "soulDrain" {
            let mut drain = effect.clone();
            drain.effect.outset = case.outset;
            let settings = &mut drain.effect.shader;
            Arc::make_mut(settings).scale = EffectScale::default();
            let depleted = capture(Some(&drain), 1.45);
            let mut ink = 0;
            let mut holes = 0;
            let mut surviving = 0;
            for (i, (pixel, original)) in depleted
                .chunks_exact(4)
                .zip(baseline.chunks_exact(4))
                .enumerate()
            {
                let native = difference(&original[..3], &blank[..3]);
                if bounds.contains(Point::new((i % 768) as f32, (i / 768) as f32)) && native > 420 {
                    ink += 1;
                    let remaining = difference(&pixel[..3], &blank[..3]);
                    holes += usize::from(remaining * 4 < native);
                    surviving += usize::from(remaining * 2 > native);
                }
            }
            assert!(ink > 20);
            assert!(
                holes * 20 > ink,
                "drain leaves actual voids in the source strokes"
            );
            assert!(
                surviving * 5 > ink,
                "drain leaves some of the inscription alive"
            );
        }
        if case.name == "blackTide" {
            let mut tide = effect.clone();
            tide.effect.outset = case.outset;
            let settings = &mut tide.effect.shader;
            Arc::make_mut(settings).scale = EffectScale::default();
            let advancing = capture(Some(&tide), 0.72);
            let region_energy = |frame: &[u8], start: f32, end: f32| -> u64 {
                frame
                    .chunks_exact(4)
                    .zip(baseline.chunks_exact(4))
                    .enumerate()
                    .filter(|(i, (_, original))| {
                        let p = Point::new((i % 768) as f32, (i / 768) as f32);
                        bounds.contains(p)
                            && p.x >= bounds.x + bounds.width * start
                            && p.x < bounds.x + bounds.width * end
                            && original[0].abs_diff(blank[0]) > 80
                    })
                    .map(|(_, (pixel, _))| pixel[..3].iter().map(|v| u64::from(*v)).sum::<u64>())
                    .sum()
            };
            assert!(
                region_energy(&advancing, 0.0, 0.30) * 4 < region_energy(&baseline, 0.0, 0.30) * 3,
                "dark pools consume ink behind their front"
            );
            assert!(
                region_energy(&advancing, 0.75, 1.0).abs_diff(region_energy(&baseline, 0.75, 1.0))
                    < region_energy(&baseline, 0.75, 1.0) / 5,
                "the front leaves unreached lettering readable"
            );
        }
        if case.name == "reverseSmoke" {
            let mut arrival = effect.clone();
            arrival.effect.outset = case.outset;
            let settings = &mut arrival.effect.shader;
            let settings = Arc::make_mut(settings);
            settings.scale = EffectScale::default();
            settings.hold = true;
            let settled = capture(Some(&arrival), 2.4);
            assert_eq!(settled, capture(Some(&arrival), 6.0));
            let ink_energy = |frame: &[u8]| -> u64 {
                frame
                    .chunks_exact(4)
                    .zip(baseline.chunks_exact(4))
                    .enumerate()
                    .filter(|(i, (_, original))| {
                        bounds.contains(Point::new((i % 768) as f32, (i / 768) as f32))
                            && original[0].abs_diff(blank[0]) > 140
                    })
                    .map(|(_, (pixel, _))| difference(&pixel[..3], &blank[..3]))
                    .sum()
            };
            assert!(
                ink_energy(&start) < ink_energy(&settled) / 3,
                "reverseSmoke begins as diffuse smoke, without flashing the original lettering: start={}, settled={}",
                ink_energy(&start),
                ink_energy(&settled)
            );
            let outside_ink = start
                .chunks_exact(4)
                .zip(baseline.chunks_exact(4))
                .enumerate()
                .filter(|(i, (smoke, plain))| {
                    !bounds.contains(Point::new((i % 768) as f32, (i / 768) as f32))
                        && difference(&smoke[..3], &plain[..3]) > 18
                })
                .count();
            assert!(
                outside_ink > 100,
                "arrival starts with visible dispersed smoke"
            );
            // Advance the swirling motion while keeping condensation at 5%.
            // Smoke must remain around the line rather than rising and falling.
            let low = capture(Some(&arrival), 0.12);
            let mut drifting = arrival.clone();
            drifting.effect.duration_ms = 12_000;
            let swirled = capture(Some(&drifting), 0.60);
            let smoke_height = |frame: &[u8]| -> f64 {
                let mut mass = 0_u64;
                let mut moment = 0_u64;
                for (i, (pixel, original)) in frame
                    .chunks_exact(4)
                    .zip(baseline.chunks_exact(4))
                    .enumerate()
                {
                    if !bounds.contains(Point::new((i % 768) as f32, (i / 768) as f32)) {
                        let weight = difference(&pixel[..3], &original[..3]);
                        mass += weight;
                        moment += weight * (i / 768) as u64;
                    }
                }
                assert!(mass > 1_000, "visible smoke outside the source ink");
                moment as f64 / mass as f64
            };
            assert!(
                (smoke_height(&swirled) - smoke_height(&low)).abs() < 12.0,
                "swirling smoke stays around its line: {} -> {}",
                smoke_height(&low),
                smoke_height(&swirled)
            );
            assert!(
                difference(&low, &swirled) > 20_000,
                "loose smoke swirls in place"
            );
            for key in ["intensity", "speed"] {
                let mut quiet = arrival.clone();
                let settings = &mut quiet.effect.shader;
                let mut params = case.params.clone();
                params[key] = 0.0.into();
                Arc::make_mut(settings).uniforms =
                    shader.uniforms(params.as_object().unwrap()).unwrap();
                assert!(difference(&capture(Some(&quiet), 0.4), &baseline) < rest_tolerance);
            }
            arrival.effect.duration_ms = 0;
            assert!(difference(&capture(Some(&arrival), 3.3), &baseline) < rest_tolerance);
            assert!(difference(&capture(Some(&arrival), 4.6), &baseline) > 20_000);
            assert!(arrival.animated(arrival.started + std::time::Duration::from_secs(10)));
            assert_eq!(
                input.stats().uploads,
                warmed_uploads,
                "smoke gathering reuses captured ink"
            );
        }
        if transformation_batch(case.name) {
            let period = match case.name {
                "crystal" | "lava" | "decay" => 3.8,
                "weightPulse" | "psionic" | "vanish" => 3.2,
                "dragonBreath" => 4.8,
                "eventHorizon" => 5.2,
                _ => unreachable!(),
            };
            let mut looping = effect.clone();
            looping.effect.duration_ms = 0;
            assert!(
                difference(
                    &capture(Some(&looping), 0.8),
                    &capture(Some(&looping), period + 0.8)
                ) < 2_000,
                "{} zero-duration cycles repeat without drift",
                case.name
            );
        }
        if case.name == "vanish" {
            let mut modes = Vec::new();
            for variant in [2.0, 3.0, 4.0] {
                let mut transition = effect.clone();
                let settings = &mut transition.effect.shader;
                let settings = Arc::make_mut(settings);
                let mut params = case.params.clone();
                params["variant"] = variant.into();
                settings.uniforms = shader.uniforms(params.as_object().unwrap()).unwrap();
                settings.hold = true;
                modes.push(capture(Some(&transition), 0.7));
                let end = capture(Some(&transition), 2.4);
                if variant == 3.0 {
                    let remaining: u64 = end
                        .chunks_exact(4)
                        .zip(blank.chunks_exact(4))
                        .enumerate()
                        .filter(|(i, _)| {
                            bounds.contains(Point::new((i % 768) as f32, (i / 768) as f32))
                        })
                        .map(|(_, (pixel, background))| difference(pixel, background))
                        .sum();
                    assert!(
                        remaining < 2_000,
                        "held departure stays hidden: {remaining}"
                    );
                } else {
                    assert!(
                        difference(&end, &baseline) < rest_tolerance,
                        "arrival and cycle finish in place"
                    );
                }
                assert_eq!(
                    end,
                    capture(Some(&transition), 6.0),
                    "held transition freezes"
                );
            }
            assert!(difference(&modes[0], &modes[1]) > 2_000);
            assert!(difference(&modes[1], &modes[2]) > 2_000);
        }
        // Dissolve/rebuild effects must not hold an invisible final frame.
        if case.name == "rootbind" {
            let mut held = effect.clone();
            let settings = &mut held.effect.shader;
            Arc::make_mut(settings).hold = true;
            Arc::make_mut(settings).fade_out_ms = 0;
            let grown = capture(Some(&held), 2.4);
            assert!(
                difference(&grown, &baseline) > 20_000,
                "grown roots remain visible"
            );
            assert_eq!(grown, capture(Some(&held), 6.0), "held roots stop growing");
            assert!(!held.animated(held.started + std::time::Duration::from_secs(6)));
            assert_eq!(
                input.stats().uploads,
                warmed_uploads,
                "growth reuses its captured glyphs"
            );
        }
        if case.name != "rootbind"
            && (attention_burst(case.name)
                || anchored_intro(case.name)
                || shader.fragments_per_glyph > 0
                || matches!(
                    case.name,
                    "liquefy"
                        | "inkBloom"
                        | "smoke"
                        | "reverseSmoke"
                        | "teleport"
                        | "emphasis"
                        | "tide"
                        | "elastic"
                        | "quake"
                        | "explode"
                        | "implode"
                ))
        {
            let mut held = effect.clone();
            let settings = &mut held.effect.shader;
            let settings = Arc::make_mut(settings);
            settings.hold = true;
            settings.fade_out_ms = 0;
            let held_end = capture(Some(&held), 2.4);
            assert!(
                difference(&held_end, &baseline) < rest_tolerance,
                "{} held finish restores its letters",
                case.name
            );
        }
        let end = capture(Some(&effect), 2.4);
        assert_eq!(end, baseline, "{} removal restores native text", case.name);
        assert!(!effect.animated(effect.started + std::time::Duration::from_millis(2400)));
    }
}

/// Keep the opt-in GPU fixtures exhaustive and valid in ordinary, GPU-free CI.
#[test]
fn catalogue_fixtures_cover_components_and_reflect_their_uniforms() {
    let package = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../packages/text-effects");
    let expected: std::collections::BTreeSet<_> = std::fs::read_dir(package)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "ts"))
        .filter_map(|path| {
            let name = path.file_stem().unwrap().to_str().unwrap();
            (!name.starts_with('_') && !matches!(name, "all" | "load" | "types"))
                .then(|| name.to_owned())
        })
        .collect();
    let cases = cases();
    let actual: std::collections::BTreeSet<_> =
        cases.iter().map(|case| case.name.to_owned()).collect();
    assert_eq!(actual, expected, "every public preset needs a GPU fixture");
    assert_eq!(actual.len(), cases.len(), "fixture names must be unique");
    for case in cases {
        let shader = Shader::compile(case.name, case.source).unwrap();
        shader.uniforms(case.params.as_object().unwrap()).unwrap();
    }
}

#[test]
#[ignore = "requires hardware GPU; run serialized with the inline effect GPU tests"]
fn particle_emitters_animate_and_density_zero_disables_symbols() {
    use iced::advanced::text::Renderer as _;
    let mut renderer = iced::futures::executor::block_on(<iced_wgpu::Renderer as Headless>::new(
        Font::MONOSPACE,
        Pixels(16.0),
        Some("wgpu"),
    ))
    .unwrap();
    register_renderer(&renderer);
    let paragraph = text::Paragraph::with_text(api::Text {
        content: "Poisoned",
        bounds: Size::new(400.0, 60.0),
        size: Pixels(40.0),
        line_height: api::LineHeight::default(),
        font: Font::MONOSPACE,
        align_x: api::Alignment::Left,
        align_y: iced::alignment::Vertical::Top,
        shaping: api::Shaping::Advanced,
        wrapping: api::Wrapping::None,
    });
    let input =
        Input::from_paragraph(&paragraph, Rectangle::with_size(paragraph.min_bounds())).unwrap();
    let viewport = Rectangle::with_size(Size::new(480.0, 240.0));
    let at = iced::Point::new(120.0, 150.0);
    let bounds = Rectangle::new(at, paragraph.min_bounds());
    for (name, source, knob, base, bright, accent) in [
        (
            "fire",
            include_str!("../../packages/text-effects/fire.wgsl"),
            "embers",
            [0.93, 0.21, 0.04, 1.0],
            [1.0, 0.7, 0.1, 1.0],
            [1.0, 0.95, 0.75, 1.0],
        ),
        (
            "poison",
            include_str!("../../packages/text-effects/cloud.wgsl"),
            "particles",
            [0.2, 0.4, 0.14, 1.0],
            [0.54, 0.89, 0.25, 1.0],
            [0.85, 1.0, 0.61, 1.0],
        ),
    ] {
        let shader = Shader::compile(name, source).unwrap();
        prewarm(&shader).unwrap();
        let mut params = serde_json::json!({"intensity":1.0,"speed":1.0,"base":base,"bright":bright,"accent":accent});
        params[knob] = 1.0.into();
        if name == "poison" {
            params["swirl"] = 1.0.into();
            params["symbol"] = 0.0.into();
        }
        let mut effect = InlineDecoration::new(
            0..8,
            TextEffect {
                shader: Arc::new(ShaderEffect {
                    pane: false,
                    scale: Default::default(),
                    capture_scale: Default::default(),
                    fade_in_ms: 0,
                    fade_out_ms: 0,
                    shader: shader.clone(),
                    uniforms: shader.uniforms(params.as_object().unwrap()).unwrap(),
                    replace: false,
                    hold: false,
                    animated: true,
                }),

                duration_ms: 0,
                outset: 112,
            },
            InlineOwner::default(),
        );
        let mut capture = |effect: &InlineDecoration, seconds: f32| {
            renderer.reset(viewport);
            assert!(draw(
                &mut renderer,
                &input,
                bounds,
                viewport,
                effect,
                effect.started + std::time::Duration::from_secs_f32(seconds)
            ));
            renderer.with_layer(viewport, |r| {
                r.fill_paragraph(&paragraph, at, iced::Color::WHITE, viewport)
            });
            Headless::screenshot(
                &mut renderer,
                Size::new(480, 240),
                1.0,
                crate::prefs::current().palette.background,
            )
        };
        let first = capture(&effect, 0.95);
        let later = capture(&effect, 3.7);
        assert_ne!(first, later, "{name} emitters advance and recycle");
        params[knob] = 0.0.into();
        let settings = &mut effect.effect.shader;
        Arc::make_mut(settings).uniforms = shader.uniforms(params.as_object().unwrap()).unwrap();
        let disabled = capture(&effect, 0.95);
        let changed = first
            .chunks_exact(4)
            .zip(disabled.chunks_exact(4))
            .filter(|(a, b)| a.iter().zip(b.iter()).any(|(a, b)| a.abs_diff(*b) > 12))
            .count();
        assert!(
            changed > 25,
            "{name} density controls independently visible particles: {changed}"
        );
        params[knob] = 1.0.into();
        let settings = &mut effect.effect.shader;
        Arc::make_mut(settings).uniforms = shader.uniforms(params.as_object().unwrap()).unwrap();
    }
    assert_eq!(
        input.stats().uploads,
        1,
        "all particle motion and uniform edits reuse the glyph input"
    );
}

#[test]
#[ignore = "requires hardware GPU; run serialized with the inline effect GPU tests"]
fn emphasis_fits_corner_anchors_and_reuses_its_high_resolution_capture() {
    let mut renderer = iced::futures::executor::block_on(<iced_wgpu::Renderer as Headless>::new(
        Font::MONOSPACE,
        Pixels(16.0),
        Some("wgpu"),
    ))
    .unwrap();
    register_renderer(&renderer);
    let shader = Shader::compile(
        "emphasis",
        include_str!("../../packages/text-effects/emphasis.wgsl"),
    )
    .unwrap();
    prewarm(&shader).unwrap();
    let paragraph = text::Paragraph::with_text(api::Text {
        content: "Now!",
        bounds: Size::new(400.0, 60.0),
        size: Pixels(24.0),
        line_height: api::LineHeight::default(),
        font: Font::MONOSPACE,
        align_x: api::Alignment::Left,
        align_y: iced::alignment::Vertical::Top,
        shaping: api::Shaping::Advanced,
        wrapping: api::Wrapping::None,
    });
    let input =
        Input::from_paragraph(&paragraph, Rectangle::with_size(paragraph.min_bounds())).unwrap();
    let screen = Rectangle::with_size(Size::new(768.0, 432.0));
    let clip = Rectangle::new(iced::Point::new(24.0, 24.0), Size::new(720.0, 384.0));
    let mut params = serde_json::json!({"intensity":1.0,"speed":1.0,"peak":8.0,"yaw":0.7,"weight":2.0,"pivot":0.5,"fit":1.0,
        "base":[0.4,0.6,0.7,1.0],"bright":[0.9,0.95,1.0,1.0],"accent":[1.0,1.0,1.0,1.0]});
    for pivot in [0.0, 0.5, 1.0] {
        params["pivot"] = pivot.into();
        let effect = InlineDecoration::new(
            0..4,
            TextEffect {
                shader: Arc::new(ShaderEffect {
                    shader: shader.clone(),
                    uniforms: shader.uniforms(params.as_object().unwrap()).unwrap(),
                    pane: true,
                    scale: Default::default(),
                    capture_scale: smudgy_session_model::text_shader::CaptureScale::new(8.0)
                        .unwrap(),
                    fade_in_ms: 0,
                    fade_out_ms: 0,
                    replace: true,
                    hold: false,
                    animated: true,
                }),

                duration_ms: 2400,
                outset: 400,
            },
            InlineOwner::default(),
        );
        for at in [
            iced::Point::new(24.0, 24.0),
            iced::Point::new(680.0, 24.0),
            iced::Point::new(24.0, 350.0),
            iced::Point::new(680.0, 350.0),
        ] {
            let mut warm_keys = None;
            for ms in [700, 1050, 1250, 1750] {
                renderer.reset(screen);
                assert!(draw(
                    &mut renderer,
                    &input,
                    Rectangle::new(at, paragraph.min_bounds()),
                    clip,
                    &effect,
                    effect.started + Duration::from_millis(ms)
                ));
                let pixels = Headless::screenshot(
                    &mut renderer,
                    Size::new(768, 432),
                    1.0,
                    iced::Color::BLACK,
                );
                if let Some(keys) = warm_keys {
                    assert_eq!(
                        input.stats().key_builds,
                        keys,
                        "animation at a fixed anchor never rebuilds the content key"
                    );
                } else {
                    warm_keys = Some(input.stats().key_builds);
                }
                let mut ink = 0;
                for (i, rgba) in pixels.chunks_exact(4).enumerate() {
                    if rgba[0] > 8 || rgba[1] > 8 || rgba[2] > 8 {
                        let x = i % 768;
                        let y = i / 768;
                        assert!(
                            (24..744).contains(&x) && (24..408).contains(&y),
                            "grown text stays clear of paint edges: pivot={pivot}, anchor={at:?}, ms={ms}, pixel=({x},{y})"
                        );
                        ink += 1;
                    }
                }
                assert!(
                    ink > 180,
                    "fitting preserves readable text even at the top edge"
                );
            }
        }
    }
    assert_eq!(
        input.stats().uploads,
        1,
        "position, pivot and time reuse the capture"
    );
    assert!(input.stats().resident_bytes <= 1024 * 1024 + 12_304);
    input.retire();
    for _ in 0..2 {
        renderer.reset(screen);
        Headless::screenshot(&mut renderer, Size::new(768, 432), 1.0, iced::Color::BLACK);
    }
    assert_eq!(input.stats().resident_bytes, 0);
}

#[test]
#[ignore = "requires hardware GPU; run serialized"]
fn typography_motion_keeps_its_feet_at_the_native_baseline() {
    use iced::advanced::text::Renderer as _;
    let mut renderer = iced::futures::executor::block_on(<iced_wgpu::Renderer as Headless>::new(
        Font::MONOSPACE,
        Pixels(16.0),
        Some("wgpu"),
    ))
    .unwrap();
    register_renderer(&renderer);
    let screen = Rectangle::with_size(Size::new(768.0, 432.0));
    let clip = screen;
    let at = iced::Point::new(260.0, 280.0);
    for size in [16.0, 32.0] {
        let paragraph = text::Paragraph::with_text(api::Text {
            content: "HMMH",
            bounds: Size::new(400.0, 60.0),
            size: Pixels(size),
            line_height: api::LineHeight::default(),
            font: Font::MONOSPACE,
            align_x: api::Alignment::Left,
            align_y: iced::alignment::Vertical::Top,
            shaping: api::Shaping::Advanced,
            wrapping: api::Wrapping::None,
        });
        renderer.reset(screen);
        renderer.fill_paragraph(&paragraph, at, iced::Color::WHITE, clip);
        let original =
            Headless::screenshot(&mut renderer, Size::new(768, 432), 1.0, iced::Color::BLACK);
        let feet = |frame: &[u8]| {
            frame
                .chunks_exact(4)
                .enumerate()
                .filter(|(_, p)| p[0] > 80 && p[1] > 80 && p[2] > 80)
                .map(|(i, _)| i / 768)
                .max()
                .expect("visible glyph ink")
        };
        let baseline = feet(&original) as i32;
        for (name, variant) in [
            ("emphasis", 0.0),
            ("tide", 0.0),
            ("elastic", 1.0),
            ("quake", 2.0),
        ] {
            let emphasis = name == "emphasis";
            let shader = Shader::compile(
                name,
                if emphasis {
                    include_str!("../../packages/text-effects/emphasis.wgsl")
                } else {
                    include_str!("../../packages/text-effects/motion.wgsl")
                },
            )
            .unwrap();
            prewarm(&shader).unwrap();
            let mut params = serde_json::json!({"intensity":1.0,"speed":1.0,
                "base":[0.4,0.6,0.7,1.0],"bright":[0.9,0.95,1.0,1.0],"accent":[1.0,1.0,1.0,1.0]});
            if emphasis {
                for (key, value) in [
                    ("peak", 3.0),
                    ("yaw", 0.35),
                    ("weight", 0.5),
                    ("pivot", 0.5),
                    ("fit", 1.0),
                ] {
                    params[key] = value.into();
                }
            } else {
                params["variant"] = variant.into();
                params["amplitude"] = 1.0.into();
            }
            let input =
                Input::from_paragraph(&paragraph, Rectangle::with_size(paragraph.min_bounds()))
                    .unwrap();
            let effect = InlineDecoration::new(
                0..4,
                TextEffect {
                    shader: Arc::new(ShaderEffect {
                        shader: shader.clone(),
                        uniforms: shader.uniforms(params.as_object().unwrap()).unwrap(),
                        pane: true,
                        scale: Default::default(),
                        capture_scale: Default::default(),
                        fade_in_ms: 0,
                        fade_out_ms: 0,
                        replace: true,
                        hold: false,
                        animated: true,
                    }),

                    duration_ms: 2400,
                    outset: 400,
                },
                InlineOwner::default(),
            );
            let mut warmed_uploads = None;
            for ms in [480, 864, 1248, 1632, 1968] {
                renderer.reset(screen);
                assert!(draw(
                    &mut renderer,
                    &input,
                    Rectangle::new(at, paragraph.min_bounds()),
                    clip,
                    &effect,
                    effect.started + Duration::from_millis(ms)
                ));
                let frame = Headless::screenshot(
                    &mut renderer,
                    Size::new(768, 432),
                    1.0,
                    iced::Color::BLACK,
                );
                let active_feet = feet(&frame) as i32;
                assert!(
                    (baseline - 5..=baseline + 6).contains(&active_feet),
                    "{name} stays around the native baseline: size={size}, ms={ms}, original={baseline}, active={active_feet}"
                );
                if let Some(uploads) = warmed_uploads {
                    assert_eq!(
                        input.stats().uploads,
                        uploads,
                        "motion reuses the warmed input"
                    );
                } else {
                    warmed_uploads = Some(input.stats().uploads);
                }
            }
            assert!(
                input.stats().uploads <= 1,
                "at most one capture, including shared paragraph reuse"
            );
            input.retire();
        }
    }
}

#[test]
fn official_shaders_validate_and_uniforms_use_wgsl_layout() {
    // Discover every shipped program: catalogue growth must not require editing
    // a second list just to receive compiler and uniform validation.
    let directory =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../packages/text-effects");
    let mut paths: Vec<_> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "wgsl")
        })
        .collect();
    paths.sort();
    assert!(
        !paths.is_empty(),
        "the effect catalogue must contain shaders"
    );
    for path in paths {
        let name = path.file_stem().unwrap().to_str().unwrap();
        let source = std::fs::read_to_string(&path).unwrap();
        let shader = Shader::compile(name, &source).unwrap();
        let mut values = serde_json::json!({"intensity": 0.8, "speed": 1.0, "base": [1.0,0.0,0.0,1.0], "bright": [1.0,1.0,0.0,1.0], "accent": [1.0,1.0,1.0,1.0]});
        for name in [
            "embers",
            "particles",
            "swirl",
            "symbol",
            "variant",
            "peak",
            "yaw",
            "weight",
            "pivot",
            "fit",
            "amplitude",
            "pieces",
            "depth",
            "spin",
            "spread",
            "mode",
            "direction",
        ] {
            if shader.fields.iter().any(|field| field.name == name) {
                values[name] = 1.0.into();
            }
        }
        let bytes = shader.uniforms(values.as_object().unwrap()).unwrap();
        assert_eq!(
            bytes.len(),
            if matches!(
                name,
                "cloud"
                    | "emphasis"
                    | "explode"
                    | "implode"
                    | "grains"
                    | "kinetic"
                    | "ribbons"
                    | "ink"
            ) {
                80
            } else {
                64
            }
        );
        let offset = if matches!(
            name,
            "cloud" | "emphasis" | "explode" | "implode" | "grains" | "kinetic" | "ribbons" | "ink"
        ) {
            32
        } else {
            16
        };
        assert_eq!(&bytes[offset..offset + 4], &1.0_f32.to_le_bytes());
        let mut invalid = values.as_object().unwrap().clone();
        invalid.insert("typo".into(), 1.into());
        assert!(shader.uniforms(&invalid).unwrap_err().contains("typo"));
        invalid.remove("typo");
        invalid.remove("speed");
        assert!(shader.uniforms(&invalid).unwrap_err().contains("speed"));
        invalid.insert("speed".into(), "fast".into());
        assert!(shader.uniforms(&invalid).is_err());
    }
}

#[test]
#[ignore = "requires hardware GPU; run serialized"]
fn crystal_handoff_has_no_pixel_translation_and_reuses_warm_capture() {
    use super::alignment_tests::{best_translation, decoration, renderer};
    let mut renderer = renderer();
    let shader = Shader::compile(
        "aligned-crystal.wgsl",
        include_str!("../../packages/text-effects/transmuted-ink.wgsl"),
    )
    .unwrap();
    prewarm(&shader).unwrap();
    let paragraph = text::Paragraph::with_text(api::Text {
        content: "Hollow crystal lettering.",
        bounds: Size::new(500.0, 40.0),
        size: Pixels(16.0),
        line_height: api::LineHeight::default(),
        font: Font::with_name("Geist Mono"),
        align_x: api::Alignment::Left,
        align_y: iced::alignment::Vertical::Top,
        shaping: api::Shaping::Advanced,
        wrapping: api::Wrapping::None,
    });
    let region = Rectangle::with_size(paragraph.min_bounds());
    let input = Input::from_paragraph(&paragraph, region).unwrap();
    let screen = Rectangle::with_size(Size::new(640.0, 240.0));
    let effect = decoration(
        &shader,
        2.0,
        false,
        serde_json::json!({
            "intensity":1.0, "speed":1.0, "variant":0.0,
            "base":[0.16,0.31,0.54,1.0], "bright":[0.56,0.85,0.93,1.0], "accent":[1.0,0.97,1.0,1.0],
        }),
    );
    for dpi in [1.0, 1.25, 2.0] {
        for phase in [0.0, 0.35, 0.7] {
            let at = Point::new(40.0 + phase, 40.0 + phase);
            renderer.reset(screen);
            renderer.fill_paragraph(&paragraph, at, iced::Color::WHITE, screen);
            let native =
                Headless::screenshot(&mut renderer, Size::new(640, 240), dpi, iced::Color::BLACK);
            let mut warm = None;
            for ms in [0, 1, 2399, 2400, 3000] {
                renderer.reset(screen);
                assert!(draw(
                    &mut renderer,
                    &input,
                    Rectangle::new(at, region.size()),
                    screen,
                    &effect,
                    effect.started + Duration::from_millis(ms)
                ));
                let frame = Headless::screenshot(
                    &mut renderer,
                    Size::new(640, 240),
                    dpi,
                    iced::Color::BLACK,
                );
                assert_eq!(
                    best_translation(&native, &frame),
                    (0, 0),
                    "dpi={dpi} phase={phase} ms={ms}"
                );
                let stats = input.stats();
                if let Some((keys, uploads)) = warm {
                    assert_eq!(
                        (stats.key_builds, stats.uploads),
                        (keys, uploads),
                        "warm playback does not recapture"
                    );
                } else {
                    warm = Some((stats.key_builds, stats.uploads));
                }
            }
        }
    }
}

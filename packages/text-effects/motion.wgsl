// Real shaped-glyph quads: a travelling tide, rubber rebound, and two seismic hits.
const TEXT_FRAGMENTS: u32 = 1u;
struct MotionParameters { intensity: f32, speed: f32, variant: f32, amplitude: f32,
    base: vec4f, bright: vec4f, accent: vec4f }
@group(1) @binding(0) var<uniform> params: MotionParameters;
fn motionPhase() -> f32 {
    if params.speed <= 0.0 { return 0.0; }
    if text.duration > 0.0 { return text.progress; }
    return fract(text.time * params.speed / 2.4);
}
fn motionStrength() -> f32 { return params.intensity * params.amplitude * text.effect_scale; }
fn effect(p: vec2f) -> vec4f {
    let phase = motionPhase();
    if phase <= 0.0 || phase >= 0.98 || motionStrength() == 0.0 || glyphsTruncated() {
        return sampleText(p);
    }
    return vec4f(0);
}
fn textVertex(instance: u32, corner: vec2f) -> SmudgyFragment {
    let phase = motionPhase();
    if phase <= 0.0 || phase >= 0.98 || motionStrength() <= 0.0 || glyphsTruncated() {
        return SmudgyFragment(vec2f(0), vec2f(0), 1.0, 0.0);
    }
    let glyph = glyphAt(instance);
    // Advance cells partition the captured paragraph, including italic overhangs
    // at the two outer edges. Shaping, ligatures and colours come from the host.
    var lo = glyph.advance.xy;
    var hi = lo + vec2f(glyph.advance.z, text.text_size.y);
    if instance + 1u < glyphCount() { hi.x = glyphAt(instance + 1u).advance.x; }
    lo.y = -2.0; hi.y = text.text_size.y + 2.0;
    if instance == 0u { lo.x = -2.0; }
    if instance + 1u == glyphCount() { hi.x = text.text_size.x + 2.0; }
    let source = mix(lo, hi, corner);
    let center = vec2f((lo.x + hi.x) * 0.5, glyphBaseline(instance));
    let strength = min(motionStrength(), 2.0);
    let x = center.x / max(text.text_size.x, 1.0);
    let h = text.text_size.y;
    var offset = vec2f(0);
    var stretch = vec2f(1);
    var angle = 0.0;
    var globalStretch = vec2f(1);
    if params.variant < 0.5 {
        let envelope = sin(clamp(phase / 0.98, 0.0, 1.0) * 3.141593);
        let wave = x * 7.0 - phase * (9.0 + 5.0 * params.speed);
        let crest = sin(wave);
        offset = vec2f(cos(wave) * h * 0.035, -crest * h * 0.10) * envelope * strength;
        stretch = vec2f(1.0 - crest * 0.04 * envelope * strength, 1.0 + crest * 0.12 * envelope * strength);
        angle = cos(wave) * 0.10 * envelope * strength;
    } else if params.variant < 1.5 {
        let gather = sin(clamp(phase / 0.19, 0.0, 1.0) * 3.141593);
        let q = max(phase - 0.19, 0.0) / 0.79;
        let rebound = sin(q * (22.0 + 9.0 * params.speed)) * exp(-q * 5.5) * (1.0 - smoothstep(0.80, 0.98, phase));
        globalStretch = vec2f(1.0 + (gather * 0.20 + rebound * 0.66) * strength,
            1.0 + (-gather * 0.38 - rebound * 0.31) * strength);
        let ripple = sin(q * (24.0 + 8.0 * params.speed) - x * 5.0) * exp(-q * 6.0)
            * smoothstep(0.19, 0.25, phase) * (1.0 - smoothstep(0.78, 0.98, phase));
        offset.y = ripple * h * 0.06 * strength;
        angle = ripple * 0.08 * strength;
        stretch.y = 1.0 + ripple * 0.15 * strength;
    } else {
        let q = max(phase - 0.16, 0.0);
        let q2 = max(phase - 0.49, 0.0);
        let hit = exp(-q * 15.0) * step(0.16, phase);
        let after = exp(-q2 * 19.0) * step(0.49, phase) * 0.45;
        let settle = 1.0 - smoothstep(0.70, 0.98, phase);
        let tick = phase * (100.0 + 60.0 * params.speed);
        let a = sin(floor(tick) * 2.399 + f32(instance) * 2.71);
        let b = sin((floor(tick) + 1.0) * 2.399 + f32(instance) * 2.71);
        let jitter = mix(a, b, smoothstep(0.0, 1.0, fract(tick)));
        let impact = (hit + after) * settle * strength;
        offset = vec2f(jitter * h * 0.16, jitter * h * 0.065) * impact;
        angle = jitter * impact * 0.12;
        stretch = vec2f(1.0 + abs(jitter) * impact * 0.18, 1.0 - abs(jitter) * impact * 0.20);
        globalStretch.y = 1.0 - (hit + after) * 0.22 * settle * strength;
    }
    globalStretch = max(globalStretch, vec2f(0.40));
    globalStretch.x = min(globalStretch.x, max(1.0, (text.surface.x - 8.0) / max(text.text_size.x, 1.0)));
    let word = vec2f(text.text_size.x * 0.5, textBaseline());
    let size = (text.text_size + vec2f(4)) * globalStretch;
    let paintLo = text.paint_offset + size * 0.5 + vec2f(4);
    let paintHi = text.paint_offset + text.surface - size * 0.5 - vec2f(4);
    let fitted = clamp(word, min(paintLo, paintHi), max(paintLo, paintHi));
    let destination = vec2f(mix(word.x, fitted.x, smoothstep(0.0, 0.06, phase) * (1.0 - smoothstep(0.91, 0.98, phase))), word.y);
    let local = (source - center) * stretch;
    let rotated = vec2f(local.x * cos(angle) - local.y * sin(angle), local.x * sin(angle) + local.y * cos(angle));
    let position = destination + (center - word) * globalStretch + rotated * globalStretch + offset;
    let visible = phase > 0.0 && phase < 0.98 && motionStrength() > 0.0 && !glyphsTruncated();
    return SmudgyFragment(position, source, 1.0, select(0.0, 1.0, visible));
}
fn textFragment(color: vec4f, source: vec2f, instance: u32) -> vec4f {
    // A restrained crest highlight, while preserving styled source ink.
    let phase = motionPhase();
    let crest = pow(max(0.0, sin(source.x / max(text.text_size.x, 1.0) * 7.0 - phase * 14.0)), 4.0);
    let tint = crest * sin(phase * 3.141593) * 0.13 * min(motionStrength(), 1.0);
    return vec4f(mix(color.rgb, params.accent.rgb * color.a, tint), color.a);
}

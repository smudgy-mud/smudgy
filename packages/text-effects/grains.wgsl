// Fine pieces of the actual captured strokes. No particle texture or CPU simulation.
const TEXT_FRAGMENTS: u32 = 64u;
struct Parameters { intensity: f32, speed: f32, variant: f32, amplitude: f32,
    mode: f32, direction: f32, base: vec4f, bright: vec4f, accent: vec4f }
@group(1) @binding(0) var<uniform> params: Parameters;
fn gh(n: f32) -> f32 { return fract(sin(n * 127.1 + text.seed) * 43758.5453); }
fn gp() -> f32 {
    if params.speed <= 0.0 { return 0.0; }
    return select(fract(text.time * params.speed / 2.4), text.progress, text.duration > 0.0);
}
// Arrival 0, departure 1, cycle 2. Departure+hold deliberately holds dispersed ink.
fn dispersion() -> f32 {
    let p = gp();
    if params.intensity <= 0.0 || params.speed <= 0.0 { return 0.0; }
    if params.mode < 0.5 { return 1.0 - smoothstep(0.06, 0.80, p); }
    if params.mode < 1.5 { return smoothstep(0.08, 0.85, p); }
    return smoothstep(0.05, 0.44, p) * (1.0 - smoothstep(0.56, 0.97, p));
}
fn effect(p: vec2f) -> vec4f {
    if dispersion() <= 0.0001 || params.amplitude <= 0.0 { return sampleText(p); }
    return vec4f(0);
}
fn textVertex(instance: u32, corner: vec2f) -> SmudgyFragment {
    let glyphIndex = instance / TEXT_FRAGMENTS;
    let piece = instance % TEXT_FRAGMENTS;
    let d = dispersion();
    if d <= 0.0001 || params.amplitude <= 0.0 || (glyphsTruncated() && glyphIndex > 0u) {
        return SmudgyFragment(vec2f(0), vec2f(0), 1.0, 0.0);
    }
    let glyph = glyphAt(glyphIndex);
    var lo = vec2f(glyph.advance.x, -2);
    var hi = vec2f(glyph.advance.x + glyph.advance.z, text.text_size.y + 2.0);
    if glyphIndex + 1u < glyphCount() { hi.x = glyphAt(glyphIndex + 1u).advance.x; }
    if glyphIndex == 0u { lo.x = -2.0; }
    if glyphIndex + 1u == glyphCount() { hi.x = text.text_size.x + 2.0; }
    if glyphsTruncated() { lo = vec2f(-2); hi = text.text_size + vec2f(2); }
    let tile = vec2f(f32(piece % 8u), f32(piece / 8u));
    let cell = (hi - lo) / 8.0;
    let source = lo + (tile + corner) * cell;
    let center = lo + (tile + vec2f(0.5)) * cell;
    let seed = f32(instance) + 13.0;
    let power = params.intensity * params.amplitude * text.effect_scale;
    var localD = clamp(d * (1.0 + (gh(seed) - 0.5) * 0.3), 0.0, 1.0);
    var offset = vec2f(0);
    var scale = vec2f(1);
    var angle = 0.0;
    if params.variant < 0.5 {
        // A travelling gust: strokes lean before tearing into colour-preserving grains.
        let front = center.x / max(text.text_size.x, 1.0);
        localD = smoothstep(front * 0.15, 0.85 + front * 0.15, d);
        let travel = pow(localD, 1.5) * max(text.surface.x * 0.65, 180.0) * power;
        let curl = sin(localD * (4.0 + params.speed) + gh(seed + 1.0) * 6.283185);
        offset = vec2f(params.direction * travel * (0.5 + gh(seed + 2.0)),
            curl * localD * text.text_size.y * (0.5 + gh(seed + 3.0) * 2.0) * power);
        offset.x += (textBaseline() - center.y) * sin(localD * 3.141593) * 0.3 * params.direction;
        scale = vec2f(1.0 + sin(localD * 3.141593) * 0.6, 1.0 - localD * 0.55);
        angle = curl * localD * 1.8;
    } else {
        // Bottom grains lose support first. Each settles into a shallow pile under its glyph.
        let bottom = clamp(center.y / max(textBaseline(), 1.0), 0.0, 1.0);
        localD = smoothstep((1.0 - bottom) * 0.20, 0.82 + (1.0 - bottom) * 0.18, d);
        let pile = vec2f((lo.x + hi.x) * 0.5 + (gh(seed + 1.0) - 0.5) * (hi.x - lo.x) * 1.5,
            textBaseline() + 2.0 - pow(1.0 - abs(gh(seed + 1.0) * 2.0 - 1.0), 2.0) * 4.0 - gh(seed + 2.0) * 2.0);
        offset = (pile - center) * localD * min(power, 2.0);
        // A tiny settling bounce, never a snowing field above the word.
        offset.y -= sin(localD * 3.141593) * gh(seed + 4.0) * 3.0;
        scale = mix(vec2f(1), vec2f(0.75, 0.65), localD);
        angle = localD * (gh(seed + 3.0) - 0.5) * 3.0;
    }
    let local = (source - center) * scale;
    let rotated = vec2f(local.x * cos(angle) - local.y * sin(angle), local.x * sin(angle) + local.y * cos(angle));
    return SmudgyFragment(center + offset + rotated, source, 1.0, 1.0);
}
fn textFragment(color: vec4f, source: vec2f, instance: u32) -> vec4f {
    // Shade the pile enough to read its granular relief, keeping the source hue.
    let shade = 1.0 - dispersion() * gh(f32(instance) + 6.0) * 0.25;
    return vec4f(color.rgb * shade, color.a);
}

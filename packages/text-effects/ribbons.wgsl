// Angular fracture and curling ink ribbons, using real source-coloured glyph pieces.
const TEXT_FRAGMENTS: u32 = 64u;
struct Parameters { intensity: f32, speed: f32, variant: f32, amplitude: f32,
    mode: f32, base: vec4f, bright: vec4f, accent: vec4f }
@group(1) @binding(0) var<uniform> params: Parameters;
fn rh(n: f32) -> f32 { return fract(sin(n * 127.1 + text.seed) * 43758.5453); }
fn rp() -> f32 {
    if params.speed <= 0.0 { return 0.0; }
    return select(fract(text.time * params.speed / 2.4), text.progress, text.duration > 0.0);
}
fn rd() -> f32 {
    let p = rp();
    if params.intensity * params.amplitude <= 0.0 || params.speed <= 0.0 { return 0.0; }
    if params.mode < 0.5 { return 1.0 - smoothstep(0.04, 0.90, p); }
    if params.mode < 1.5 { return smoothstep(0.05, 0.90, p); }
    return smoothstep(0.02, 0.43, p) * (1.0 - smoothstep(0.55, 0.98, p));
}
fn rb(instance: u32) -> vec4f {
    let index = instance / TEXT_FRAGMENTS;
    let glyph = glyphAt(index);
    var lo = vec2f(glyph.advance.x, -2);
    var hi = vec2f(glyph.advance.x + glyph.advance.z, text.text_size.y + 2.0);
    if index + 1u < glyphCount() { hi.x = glyphAt(index + 1u).advance.x; }
    if index == 0u { lo.x = -2.0; }
    if index + 1u == glyphCount() { hi.x = text.text_size.x + 2.0; }
    if glyphsTruncated() { lo = vec2f(-2); hi = text.text_size + vec2f(2); }
    return vec4f(lo, hi);
}
fn effect(p: vec2f) -> vec4f {
    if rd() <= 0.0001 { return sampleText(p); }
    return vec4f(0);
}
fn textVertex(instance: u32, corner: vec2f) -> SmudgyFragment {
    let d = rd();
    let piece = instance % TEXT_FRAGMENTS;
    if d <= 0.0001 || (glyphsTruncated() && instance / TEXT_FRAGMENTS > 0u)
        || (params.variant < 0.5 && piece >= 16u) {
        return SmudgyFragment(vec2f(0), vec2f(0), 1.0, 0.0);
    }
    let bounds = rb(instance);
    var tile = vec2f(f32((piece / 2u) % 4u), f32(piece / 8u));
    var grid = vec2f(4, 2);
    if params.variant >= 0.5 { tile = vec2f(f32(piece % 8u), f32(piece / 8u)); grid = vec2f(8); }
    let cell = (bounds.zw - bounds.xy) / grid;
    let source = bounds.xy + (tile + corner) * cell;
    let center = bounds.xy + (tile + vec2f(0.5)) * cell;
    let seed = f32(instance) + 51.0;
    let power = min(params.intensity * params.amplitude * text.effect_scale, 2.5);
    var position = source;
    var w = 1.0;
    if params.variant < 0.5 {
        let word = vec2f(text.text_size.x * 0.5, textBaseline() - text.text_size.y * 0.35);
        let theta = rh(seed) * 6.283185;
        let travel = pow(d, 1.4) * power * text.text_size.y * (1.2 + rh(seed + 2.0) * 4.0);
        let angle = (rh(seed + 1.0) - 0.5) * d * 7.0;
        let local = source - center;
        let rotated = vec2f(local.x * cos(angle) - local.y * sin(angle), local.x * sin(angle) + local.y * cos(angle));
        w = max(0.6, 1.0 + (rh(seed + 3.0) - 0.5) * d * power);
        position = word + (center - word + rotated + vec2f(cos(theta), sin(theta)) * travel) / w;
    } else {
        // Adjacent segments form a continuous ribbon; wave phase follows source X.
        let band = f32(piece / 8u);
        let u = (source.x - bounds.x) / max(bounds.z - bounds.x, 1.0);
        let curl = d * (u * 5.0 + band * 0.85 + rp() * params.speed * 1.5);
        let drift = vec2f((source.x - text.text_size.x * 0.5) * d * 0.35,
            sin(curl) * d * text.text_size.y * (0.35 + band * 0.055));
        let local = source - center;
        position = center + vec2f(local.x * (1.0 + d * 0.35), local.y * (1.0 - d * 0.65))
            + drift * power + vec2f(sin(band * 1.7) * d * text.text_size.y * 0.4, band * d * 1.1);
    }
    return SmudgyFragment(position, source, w, 1.0);
}
fn textFragment(color: vec4f, source: vec2f, instance: u32) -> vec4f {
    if params.variant < 0.5 {
        let bounds = rb(instance);
        let cell = (bounds.zw - bounds.xy) / vec2f(4, 2);
        let uv = fract((source - bounds.xy) / cell);
        let triangle = select(uv.x + uv.y <= 1.0, uv.x + uv.y > 1.0, instance % 2u == 1u);
        if !triangle { return vec4f(0); }
    }
    let sheen = sin(rd() * 3.141593) * 0.20;
    return vec4f(mix(color.rgb, params.bright.rgb * color.a, sheen), color.a);
}

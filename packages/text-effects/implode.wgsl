// Captured glyph chunks converge from all four pane corners at one shared impact.
const TEXT_FRAGMENTS: u32 = 16u;
struct Parameters { intensity: f32, speed: f32, pieces: f32, depth: f32,
    spin: f32, spread: f32, base: vec4f, bright: vec4f, accent: vec4f }
@group(1) @binding(0) var<uniform> params: Parameters;
fn ih(n: f32) -> f32 { return fract(sin(n * 127.1 + text.seed) * 43758.5453); }
fn ip() -> f32 {
    if params.speed <= 0.0 { return 1.0; }
    return select(fract(text.time * params.speed / 2.4), text.progress, text.duration > 0.0);
}
fn effect(p: vec2f) -> vec4f {
    let phase = ip();
    if params.intensity <= 0.0 || phase >= 0.90 { return sampleText(p); }
    let age = max(phase - 0.68, 0.0);
    if phase < 0.68 || age > 0.16 { return vec4f(0); }
    let center = vec2f(text.text_size.x * 0.5, textBaseline() - text.text_size.y * 0.3);
    let radius = age * max(text.text_size.y * 12.0, 200.0) * text.effect_scale;
    let ring = exp(-pow(abs(length((p - center) * vec2f(0.65, 1)) - radius) / 1.8, 2.0))
        * sin(min(age / 0.16, 1.0) * 3.141593) * exp(-age * 16.0) * params.intensity;
    let alpha = min(ring, 0.65) * params.accent.a;
    return vec4f(params.accent.rgb * alpha, alpha);
}
fn textVertex(instance: u32, corner: vec2f) -> SmudgyFragment {
    let phase = ip();
    let glyphIndex = instance / TEXT_FRAGMENTS;
    let piece = instance % TEXT_FRAGMENTS;
    let side = u32(clamp(params.pieces, 2.0, 4.0));
    if phase >= 0.90 || params.intensity <= 0.0 || piece >= side * side
        || (glyphsTruncated() && glyphIndex > 0u) {
        return SmudgyFragment(vec2f(0), vec2f(0), 1.0, 0.0);
    }
    let glyph = glyphAt(glyphIndex);
    var lo = vec2f(glyph.advance.x, -2);
    var hi = vec2f(glyph.advance.x + glyph.advance.z, text.text_size.y + 2.0);
    if glyphIndex + 1u < glyphCount() { hi.x = glyphAt(glyphIndex + 1u).advance.x; }
    if glyphIndex == 0u { lo.x = -2.0; }
    if glyphIndex + 1u == glyphCount() { hi.x = text.text_size.x + 2.0; }
    if glyphsTruncated() { lo = vec2f(-2); hi = text.text_size + vec2f(2); }
    let tile = vec2f(f32(piece % side), f32(piece / side));
    let cell = (hi - lo) / f32(side);
    let source = lo + (tile + corner) * cell;
    let center = lo + (tile + vec2f(0.5)) * cell;
    let seed = f32(instance) + 23.0;
    let q = clamp(phase / 0.68, 0.0, 1.0);
    let remaining = pow(1.0 - q, 0.65 + ih(seed) * 1.65);
    let paneCorner = vec2f(f32(instance % 2u), f32((instance / 2u) % 2u));
    let launch = text.paint_offset + mix(vec2f(12), text.surface - vec2f(12), paneCorner);
    let drift = (launch - center) * remaining * params.spread * min(params.intensity * text.effect_scale, 2.0);
    let bend = vec2f(sin(q * 3.141593), sin(q * 6.283185)) * (ih(seed + 8.0) - 0.5)
        * text.text_size.y * 2.0 * remaining;
    let angle = (ih(seed + 1.0) * 2.0 - 1.0) * remaining * params.spin * 10.0;
    let local = source - center;
    let rotated = vec2f(local.x * cos(angle) - local.y * sin(angle), local.x * sin(angle) + local.y * cos(angle));
    let z = (ih(seed + 2.0) - 0.2) * remaining * params.depth * 1.3;
    let w = max(1.0 + z, 0.45);
    let impact = sin(clamp((phase - 0.68) / 0.22, 0.0, 1.0) * 3.141593)
        * exp(-max(phase - 0.68, 0.0) * 9.0) * 0.48 * params.intensity * text.effect_scale;
    let pivot = vec2f(text.text_size.x * 0.5, textBaseline());
    let position = pivot + (center - pivot) * (1.0 + impact) + (rotated + drift + bend) * (1.0 + impact) / w;
    return SmudgyFragment(position, source, w, smoothstep(0.0, 0.07, phase));
}
fn textFragment(color: vec4f, source: vec2f, instance: u32) -> vec4f {
    let flash = exp(-max(ip() - 0.68, 0.0) * 30.0) * step(0.68, ip()) * 0.65;
    return vec4f(mix(color.rgb, params.accent.rgb * color.a, flash), color.a);
}

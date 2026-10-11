// Actual captured ink fragments, not substitute particle shapes. Their analytical
// trajectories have signed depth and independent three-axis rotation.
const TEXT_FRAGMENTS: u32 = 16u;
struct ExplosionParameters { intensity: f32, speed: f32, pieces: f32, depth: f32,
    spin: f32, spread: f32, base: vec4f, bright: vec4f, accent: vec4f }
@group(1) @binding(0) var<uniform> params: ExplosionParameters;
fn explodeHash(n: f32) -> f32 { return fract(sin(n * 127.1 + text.seed) * 43758.5453); }
fn explodePhase() -> f32 {
    if params.speed <= 0.0 { return 0.0; }
    if text.duration > 0.0 { return text.progress; }
    return fract(text.time * params.speed / 2.4);
}
fn effect(p: vec2f) -> vec4f {
    let phase = explodePhase();
    if params.intensity == 0.0 || phase <= 0.0 || phase >= 0.98 { return sampleText(p); }
    let word = text.text_size * 0.5;
    if phase < 0.18 {
        let gather = sin(phase / 0.18 * 3.141593) * params.intensity;
        let zoom = vec2f(1.0 - gather * 0.08, 1.0 - gather * 0.22);
        return sampleText(word + (p - word) / zoom);
    }
    let restore = smoothstep(0.80, 0.98, phase);
    var color = vec4f(0);
    if restore > 0.0 { color = sampleText(p) * restore; }
    // A short, local shockfront gives the moment of breakage a clear beat.
    let age = (phase - 0.18) / 0.70;
    if age > 0.20 { return color; }
    let radius = age * max(text.text_size.y * 7.0, 120.0) * text.effect_scale;
    let d = length((p - word) * vec2f(0.55, 1.0));
    let ring = exp(-pow((d - radius) / 2.2, 2.0)) * exp(-age * 20.0)
        * smoothstep(0.0, 0.015, age) * 0.6 * params.intensity;
    let alpha = min(ring, 0.7) * params.accent.a;
    return vec4f(params.accent.rgb * alpha + color.rgb * (1.0 - alpha), alpha + color.a * (1.0 - alpha));
}
fn textVertex(instance: u32, corner: vec2f) -> SmudgyFragment {
    let glyphIndex = instance / TEXT_FRAGMENTS;
    let piece = instance % TEXT_FRAGMENTS;
    let side = u32(clamp(params.pieces, 2.0, 4.0));
    let phase = explodePhase();
    if piece >= side * side || phase < 0.18 || phase >= 0.91 || params.intensity <= 0.0
        || (glyphsTruncated() && glyphIndex > 0u) {
        return SmudgyFragment(vec2f(0), vec2f(0), 1.0, 0.0);
    }
    let glyph = glyphAt(glyphIndex);
    var lo = vec2f(glyph.advance.x, -2.0);
    var hi = vec2f(glyph.advance.x + glyph.advance.z, text.text_size.y + 2.0);
    if glyphIndex + 1u < glyphCount() { hi.x = glyphAt(glyphIndex + 1u).advance.x; }
    if glyphIndex == 0u { lo.x = -2.0; }
    if glyphIndex + 1u == glyphCount() { hi.x = text.text_size.x + 2.0; }
    // Metadata is bounded to 256 shaped glyphs. Long fragments still shatter the
    // complete captured image, using a coarser whole-word grid.
    if glyphsTruncated() { lo = vec2f(-2); hi = text.text_size + vec2f(2); }
    let cell = (hi - lo) / f32(side);
    let tile = vec2f(f32(piece % side), f32(piece / side));
    let source = lo + (tile + corner) * cell;
    let center = lo + (tile + vec2f(0.5)) * cell;
    let age = max((phase - 0.18) / 0.70, 0.0);
    let flight = pow(age, 0.78) * min(params.intensity * text.effect_scale, 2.5)
        * (0.65 + params.speed * 0.35);
    let seed = f32(instance) + 17.0;
    let z = explodeHash(seed + 2.0) * 2.0 - 1.0;
    let theta = explodeHash(seed) * 6.283185;
    let radial = sqrt(max(1.0 - z * z, 0.08));
    let direction = vec3f(cos(theta) * radial, sin(theta) * radial, z);
    let camera = max(text.text_size.y * 8.0, 180.0);
    let travel = flight * (0.55 + explodeHash(seed + 4.0) * 0.7);
    let depth = direction.z * camera * params.depth * travel;
    if -depth / camera >= 0.90 { return SmudgyFragment(vec2f(0), vec2f(0), 1.0, 0.0); }
    let xy = direction.xy * max(min(text.surface.x, text.surface.y) * 0.60, 90.0) * travel * params.spread;
    let angle = (vec3f(explodeHash(seed + 6.0), explodeHash(seed + 7.0), explodeHash(seed + 8.0)) * 2.0 - 1.0)
        * travel * params.spin * 9.0;
    var local = vec3f((source - center) * (1.0 + sin(min(age * 12.0, 3.141593)) * 0.2), 0.0);
    local = vec3f(local.x, local.y * cos(angle.x), local.y * sin(angle.x));
    local = vec3f(local.x * cos(angle.y) + local.z * sin(angle.y), local.y,
        -local.x * sin(angle.y) + local.z * cos(angle.y));
    local = vec3f(local.x * cos(angle.z) - local.y * sin(angle.z),
        local.x * sin(angle.z) + local.y * cos(angle.z), local.z);
    let w = max(1.0 + (depth + local.z) / camera, 0.16);
    let word = text.text_size * 0.5;
    let position = word + (center - word + xy + local.xy) / w;
    let fade = (1.0 - smoothstep(0.65, 0.91, phase)) * (1.0 - smoothstep(0.72, 0.90, -depth / camera));
    let visible = piece < side * side && phase >= 0.18 && phase < 0.98 && params.intensity > 0.0
        && (!glyphsTruncated() || glyphIndex == 0u);
    // Fade fragments before the near plane; perspective never becomes unbounded.
    return SmudgyFragment(position, source, w, select(0.0, fade, visible));
}
fn textFragment(color: vec4f, source: vec2f, instance: u32) -> vec4f {
    let phase = explodePhase();
    let hot = exp(-max(phase - 0.18, 0.0) * 18.0) * 0.65;
    let shade = 0.60 + explodeHash(f32(instance) + 19.0) * 0.40;
    return vec4f(mix(color.rgb * shade, params.accent.rgb * color.a, hot), color.a);
}

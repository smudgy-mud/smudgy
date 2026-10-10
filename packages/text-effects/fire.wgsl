struct Parameters { intensity: f32, speed: f32, embers: f32, base: vec4f, bright: vec4f, accent: vec4f }
@group(1) @binding(0) var<uniform> params: Parameters;
fn hash(p: vec2f) -> f32 { return fract(sin(dot(p, vec2f(127.1, 311.7))) * 43758.5453); }
fn noise(p: vec2f) -> f32 {
    let i = floor(p); let f = fract(p); let t = f * f * (3.0 - 2.0 * f);
    return mix(mix(hash(i), hash(i + vec2f(1, 0)), t.x), mix(hash(i + vec2f(0, 1)), hash(i + vec2f(1, 1)), t.x), t.y);
}
fn fbm(p: vec2f) -> f32 { return noise(p) * 0.57 + noise(p * 2.03 + vec2f(13.1, 7.7)) * 0.28 + noise(p * 4.11 + vec2f(-3.2, 19.3)) * 0.15; }
// Rising tongues are seeded by the actual glyph coverage, including styled fonts.
fn effect(p: vec2f) -> vec4f {
    let time = text.time * params.speed;
    let s = text.effect_scale;
    let q = p / s;
    let height = (20.0 + 28.0 * params.intensity) * s;
    let n = fbm(vec2f(q.x * 0.075 + text.seed, q.y * 0.065 + time * 2.7));
    let curl = noise(vec2f(q.x * 0.028 + time * 0.5, q.y * 0.045 + time));
    let warp = vec2f((curl - 0.5) * 15.0, (n - 0.5) * 10.0) * s;
    var fuel = 0.0;
    // Fixed tap count, independent of font size and overflow area.
    for (var i = 0; i < 12; i++) {
        let rise = f32(i) / 11.0;
        let sample = p + warp + vec2f(sin(rise * 6.0 + time + curl * 3.0) * rise * 9.0 * s, rise * height);
        let mask = (coverage(sample) + coverage(sample + vec2f(1.8, 1.0) * s) + coverage(sample - vec2f(1.8, 1.0) * s)) / 3.0;
        fuel += mask * (1.0 - rise * 0.8) * 0.48;
    }
    let heat = max(fuel * 1.45 - (1.0 - n) * 0.68, 0.0) * params.intensity;
    let warm = mix(params.base, params.bright, smoothstep(0.15, 0.8, heat));
    let color = mix(warm, params.accent, smoothstep(0.65, 1.4, heat));
    let alpha = smoothstep(0.025, 0.65, heat) * color.a;
    var flame = vec4f(color.rgb * alpha, alpha);
    // Analytical emitter: each slot has a seeded birth phase, lifetime and
    // velocity. Recycling a slot changes its seed; no CPU or history is needed.
    for (var i = 0; i < 16; i++) {
        if f32(i) >= params.embers * 8.0 { break; }
        let id = f32(i) + text.seed * 31.0;
        let lifetime = 1.2 + hash(vec2f(id, 2.0)) * 1.3;
        let clock = time / lifetime + hash(vec2f(id, 7.0));
        let cycle = floor(clock);
        let age = fract(clock);
        let born = hash(vec2f(id, cycle));
        let x = born * text.text_size.x + sin(age * 5.0 + id) * age * 9.0 * s;
        let y = text.text_size.y * 0.55 - age * (62.0 + hash(vec2f(id, cycle + 9.0)) * 38.0) * s;
        let radius = mix(1.6, 0.8, age) * s;
        let distance = length(p - vec2f(x, y));
        let glow = (1.0 - smoothstep(radius * 0.3, radius * 2.3, distance));
        let spark = glow * sin(age * 3.14159265) * min(params.intensity, 1.0);
        let tint = mix(params.bright, params.accent, born);
        let ember = vec4f(tint.rgb * spark * tint.a, spark * tint.a);
        flame = ember + flame * (1.0 - ember.a);
    }
    var outline = coverage(p);
    for (var i = 0; i < 8; i++) {
        let angle = f32(i) * 0.785398;
        outline = max(outline, coverage(p + vec2f(cos(angle), sin(angle)) * 1.6));
    }
    // An opaque stroke also protects antialiased edges from the bright flame.
    let stroke = smoothstep(0.01, 0.12, outline) * min(params.intensity, 1.0) * color.a;
    return vec4f(mix(flame.rgb, text.background.rgb, stroke), flame.a + stroke * (1.0 - flame.a));
}

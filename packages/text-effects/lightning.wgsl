struct Parameters { intensity: f32, speed: f32, base: vec4f, bright: vec4f, accent: vec4f }
@group(1) @binding(0) var<uniform> params: Parameters;
fn hash(p: vec2f) -> f32 { return fract(sin(dot(p, vec2f(127.1, 311.7))) * 43758.5453); }
fn tinted(amount: f32, heat: f32) -> vec4f {
    let tint = mix(mix(params.base, params.bright, clamp(heat, 0.0, 1.0)), params.accent, smoothstep(0.6, 1.0, heat));
    let alpha = clamp(amount * params.intensity, 0.0, 1.0) * tint.a;
    return vec4f(tint.rgb * alpha, alpha);
}

// A strike propagates left to right, followed by two short blooms around the struck word.
fn bolt(x: f32, seed: f32) -> f32 {
    let cell = floor(x / 11.0);
    return mix(hash(vec2f(cell, seed)), hash(vec2f(cell + 1.0, seed)), fract(x / 11.0)) * 2.0 - 1.0;
}
fn effect(p: vec2f) -> vec4f {
    let clock = text.time * params.speed / 1.7;
    let t = fract(clock);
    let seed = floor(clock) + text.seed;
    let s = text.effect_scale;
    let center = text.text_size.y * 0.52;
    let start = -12.0 * s;
    let end = text.text_size.x + 12.0 * s;
    let head = mix(start, end, smoothstep(0.04, 0.34, t));
    let trail = smoothstep(0.015, 0.045, t) * (1.0 - smoothstep(0.34, 0.49, t));
    let flash = exp(-pow((t - 0.36) / 0.023, 2.0)) + exp(-pow((t - 0.43) / 0.013, 2.0)) * 0.65;
    let outsideX = max(max(start - p.x, p.x - end), 0.0) / s;
    let vertical = (p.y - center) / max(text.text_size.y * 0.26, 8.0 * s);
    let bloom = exp(-vertical * vertical - outsideX * outsideX / 256.0);
    let result = tinted(flash * bloom * 0.14, 1.0);
    if p.x < start - 8.0 * s || p.x > head + 5.0 * s || abs(p.y - text.text_size.y * 0.52) > 27.0 * s { return result; }
    let y = center + bolt(p.x / s, seed) * (4.0 + params.intensity * 3.0) * s;
    let d = abs(p.y - y) / s;
    let core = 1.0 - smoothstep(0.35, 1.05, d);
    let halo = exp(-d * 0.38) * 0.34;
    let reveal = 1.0 - smoothstep(head, head + 3.0 * s, p.x);
    let cap = exp(-length((p - vec2f(head, y)) / s) * 0.35) * (1.0 - smoothstep(0.31, 0.35, t));
    let branchY = y - (3.0 + sin(p.x / s * 0.28 + seed) * 5.0) * s;
    let branch = exp(-abs(p.y - branchY) / (0.65 * s)) * pow(hash(vec2f(floor(p.x / (24.0 * s)), seed)), 3.0) * 0.30;
    let shape = tinted(((core + halo + branch) * reveal + cap) * trail * (0.7 + flash * 0.3), core + cap);
    return shape + result * (1.0 - shape.a);
}

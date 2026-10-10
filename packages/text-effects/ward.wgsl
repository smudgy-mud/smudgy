struct Parameters { intensity: f32, speed: f32, base: vec4f, bright: vec4f, accent: vec4f }
@group(1) @binding(0) var<uniform> params: Parameters;
fn hash(p: vec2f) -> f32 { return fract(sin(dot(p, vec2f(127.1, 311.7))) * 43758.5453); }
fn tinted(amount: f32, heat: f32) -> vec4f {
    let tint = mix(mix(params.base, params.bright, clamp(heat, 0.0, 1.0)), params.accent, smoothstep(0.6, 1.0, heat));
    let alpha = clamp(amount * params.intensity, 0.0, 1.0) * tint.a;
    return vec4f(tint.rgb * alpha, alpha);
}

fn box(p: vec2f, halfsize: vec2f, radius: f32) -> f32 {
    let q = abs(p) - halfsize + vec2f(radius);
    return length(max(q, vec2f(0.0))) + min(max(q.x, q.y), 0.0) - radius;
}
// A close protective border, travelling highlights and small end runes fit a text line.
fn effect(p: vec2f) -> vec4f {
    let s = text.effect_scale;
    let time = text.time * params.speed;
    let q = p - text.text_size * 0.5;
    let axes = text.text_size * 0.5 + vec2f(6.0, 1.0) * s;
    let d = abs(box(q, axes, 3.0 * s)) / s;
    let ring = exp(-d * 1.8) * 0.42;
    let crawl = pow(0.5 + 0.5 * sin(q.x / max(axes.x, 1.0) * 3.141593 - time * 1.4 + sign(q.y) * 2.0), 8.0);
    let gleam = exp(-d * 0.9) * crawl * 0.65;
    let v = vec2f(abs(q.x) - axes.x, q.y) / s;
    let rune = exp(-abs(abs(v.x) + abs(v.y) - min(axes.y / s * 0.5, 5.0)) * 2.5) * (1.0 - smoothstep(5.0, 8.0, abs(v.y)));
    return tinted(ring + gleam + rune * 0.70, gleam + rune);
}

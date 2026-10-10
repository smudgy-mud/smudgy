struct Parameters { intensity: f32, speed: f32, base: vec4f, bright: vec4f, accent: vec4f }
@group(1) @binding(0) var<uniform> params: Parameters;
fn hash(p: vec2f) -> f32 { return fract(sin(dot(p, vec2f(127.1, 311.7))) * 43758.5453); }
fn tinted(amount: f32, heat: f32) -> vec4f {
    let tint = mix(mix(params.base, params.bright, clamp(heat, 0.0, 1.0)), params.accent, smoothstep(0.6, 1.0, heat));
    let alpha = clamp(amount * params.intensity, 0.0, 1.0) * tint.a;
    return vec4f(tint.rgb * alpha, alpha);
}

// A broad gold halo, a sweeping white gleam and starbursts outside the text.
fn effect(p: vec2f) -> vec4f {
    let phase = select(fract(text.time * params.speed / 2.8), text.progress, text.duration > 0.0);
    let s = text.effect_scale;
    let q = p - text.text_size * 0.5;
    let axes = text.text_size * 0.5 + vec2f(35.0, 35.0) * s;
    let halo = exp(-dot(q / axes, q / axes) * 1.6) * 0.52;
    let sweep = mix(-45.0 * s, text.text_size.x + 45.0 * s, phase);
    let gleam = exp(-pow((p.x + q.y * 0.65 - sweep) / (7.0 * s), 2.0));
    let gate = exp(-abs(q.y) / (35.0 * s));
    var stars = 0.0;
    for (var i = 0; i < 5; i++) {
        let id = f32(i) + text.seed;
        let point = vec2f(hash(vec2f(id, 1.0)) * text.text_size.x, text.text_size.y * 0.5 + (hash(vec2f(id, 3.0)) - 0.5) * 95.0 * s);
        let v = (p - point) / s;
        let pulse = pow(max(0.0, sin(phase * 3.141593 + f32(i) * 0.5)), 4.0);
        stars += (exp(-abs(v.x) * 1.8) * exp(-abs(v.y) / 17.0) + exp(-abs(v.y) * 1.8) * exp(-abs(v.x) / 17.0)) * pulse;
    }
    let distance = length(q / s);
    let angle = atan2(q.y, q.x);
    let rays = pow(max(0.0, sin(angle * 8.0 + phase * 0.4)), 28.0) * exp(-distance / 95.0) * 0.42;
    var result = tinted(halo + gleam * gate + stars + rays, min(gleam + stars + rays, 1.0));
    var edge = coverage(p);
    for (var i = 0; i < 8; i++) {
        let a = f32(i) * 0.785398;
        edge = max(edge, coverage(p + vec2f(cos(a), sin(a)) * 1.5));
    }
    let stroke = smoothstep(0.01, 0.12, edge) * min(params.intensity, 1.0);
    result = vec4f(mix(result.rgb, text.background.rgb, stroke), result.a + stroke * (1.0 - result.a));
    let outside = max(max(-p.x, p.x - text.text_size.x), max(-p.y, p.y - text.text_size.y));
    let edgeFade = 1.0 - smoothstep(text.outset * 0.70, max(text.outset, 1.0), outside);
    return result * edgeFade;
}

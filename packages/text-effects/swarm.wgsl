struct Parameters { intensity: f32, speed: f32, particles: f32, base: vec4f, bright: vec4f, accent: vec4f }
@group(1) @binding(0) var<uniform> params: Parameters;
fn hash(p: vec2f) -> f32 { return fract(sin(dot(p, vec2f(127.1, 311.7))) * 43758.5453); }
fn tinted(amount: f32, heat: f32) -> vec4f {
    let tint = mix(mix(params.base, params.bright, clamp(heat, 0.0, 1.0)), params.accent, smoothstep(0.6, 1.0, heat));
    let alpha = clamp(amount * params.intensity, 0.0, 1.0) * tint.a;
    return vec4f(tint.rgb * alpha, alpha);
}

// Small winged insects follow independent loops; no sprite atlas or history buffer.
fn effect(p: vec2f) -> vec4f {
    let s = text.effect_scale;
    let time = text.time * params.speed;
    var wings = 0.0;
    var bodies = 0.0;
    for (var i = 0; i < 24; i++) {
        if f32(i) >= params.particles * 12.0 { break; }
        let id = f32(i) + text.seed;
        let phase = time * (0.5 + hash(vec2f(id, 1.0)) * 0.7) + id;
        let center = vec2f(text.text_size.x * (0.5 + 0.48 * sin(phase)) + sin(phase * 3.3) * 14.0 * s,
            text.text_size.y * 0.5 + (cos(phase * 1.3) * 19.0 + sin(phase * 4.7) * 6.0) * s);
        let q = (p - center) / (s * 1.2);
        if abs(q.x) < 8.0 && abs(q.y) < 7.0 {
            let flap = 0.55 + 0.45 * abs(sin(time * 27.0 + id));
            let wing = min(length((q - vec2f(2.5 * flap, -0.9)) / vec2f(2.6 * flap, 1.6)),
                           length((q + vec2f(2.5 * flap, 0.9)) / vec2f(2.6 * flap, 1.6)));
            wings = max(wings, (1.0 - smoothstep(0.7, 1.2, wing)) * 0.8);
            bodies = max(bodies, 1.0 - smoothstep(0.6, 1.1, length(q / vec2f(1.0, 2.1))));
        }
    }
    let body = tinted(bodies * 0.95, 0.65);
    let wingAlpha = clamp(wings * 0.52 * params.intensity, 0.0, 1.0) * params.accent.a;
    let wing = vec4f(params.accent.rgb * wingAlpha, wingAlpha);
    return body + wing * (1.0 - body.a);
}

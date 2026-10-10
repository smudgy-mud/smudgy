struct Parameters { intensity: f32, speed: f32, particles: f32, swirl: f32, symbol: f32, base: vec4f, bright: vec4f, accent: vec4f }
@group(1) @binding(0) var<uniform> params: Parameters;
fn hash(p: vec2f) -> f32 { return fract(sin(dot(p, vec2f(127.1, 311.7))) * 43758.5453); }
fn noise(p: vec2f) -> f32 {
    let i = floor(p); let f = fract(p); let t = f * f * (3.0 - 2.0 * f);
    return mix(mix(hash(i), hash(i + vec2f(1, 0)), t.x), mix(hash(i + vec2f(0, 1)), hash(i + vec2f(1, 1)), t.x), t.y);
}
fn fbm(p: vec2f) -> f32 { return noise(p) * 0.57 + noise(p * 2.03 + vec2f(13.1, 7.7)) * 0.28 + noise(p * 4.11 + vec2f(-3.2, 19.3)) * 0.15; }

// Signed-distance skull with eye sockets, nose, jaw and two tooth gaps.
fn skull(p: vec2f) -> f32 {
    let head = length(p - vec2f(0.0, -0.8)) - 3.5;
    let jaw = max(abs(p.x) - 2.1, abs(p.y - 2.0) - 1.8);
    var shape = min(head, jaw);
    let eyes = min(length(p - vec2f(-1.3, -0.6)), length(p - vec2f(1.3, -0.6))) - 0.88;
    let nose = abs(p.x) + abs(p.y - 0.9) - 0.62;
    let teeth = min(max(abs(p.x - 0.7) - 0.23, 2.3 - p.y), max(abs(p.x + 0.7) - 0.23, 2.3 - p.y));
    shape = max(shape, -min(min(eyes, nose), teeth));
    return 1.0 - smoothstep(-0.3, 0.45, shape);
}
fn heart(p: vec2f) -> f32 {
    let q = vec2f(p.x, -p.y) / 3.0;
    let a = dot(q, q) - 1.0;
    return 1.0 - smoothstep(-0.07, 0.08, a * a * a - q.x * q.x * q.y * q.y * q.y);
}
fn effect(p: vec2f) -> vec4f {
    if params.intensity <= 0.0 { return vec4f(0.0); }
    let time = text.time * params.speed;
    let s = text.effect_scale;
    var density = 0.0;
    var symbols = 0.0;
    // Twelve repeating puff emitters cover the anchor. Each slot owns its own
    // emission phase, velocity, expansion and fade envelope.
    for (var i = 0; i < 12; i++) {
        let id = f32(i) + text.seed * 17.0;
        let life = 2.5 + hash(vec2f(id, 8.0)) * 2.0;
        let clock = time / life + hash(vec2f(id, 3.0));
        let age = fract(clock);
        let cycle = floor(clock);
        let random = hash(vec2f(id, cycle));
        let origin = (f32(i) + random) / 12.0 * text.text_size.x;
        let drift = sin(age * 5.0 + id) * age * 22.0 * params.swirl * s;
        let center = vec2f(origin + drift, text.text_size.y * 0.68 - age * (45.0 + random * 35.0) * s);
        let radius = mix(9.0, 25.0 + 9.0 * params.intensity, age) * s;
        let d = (p - center) / radius;
        density += max(1.0 - dot(d, d), 0.0) * sin(age * 3.14159265) * 0.48;
    }
    // Distinct skull particles have independent births, travel and lifetimes.
    // Their trajectories remain deterministic through dropped/offscreen frames.
    for (var i = 0; i < 16; i++) {
        if f32(i) >= params.particles * 8.0 { break; }
        let id = f32(i) + 71.0 + text.seed * 29.0;
        let life = 2.0 + hash(vec2f(id, 6.0)) * 2.2;
        let clock = time / life + hash(vec2f(id, 1.0));
        let age = fract(clock);
        let cycle = floor(clock);
        let random = hash(vec2f(id, cycle));
        let size = mix(0.9, 1.6, age) * s;
        let center = vec2f(random * text.text_size.x + sin(age * 4.0 + id) * age * 19.0 * params.swirl * s,
                          text.text_size.y * 0.6 - age * (58.0 + random * 30.0) * s);
        let q = (p - center) / size;
        if abs(q.x) < 5.0 && abs(q.y) < 5.0 {
            symbols = max(symbols, select(skull(q), heart(q), params.symbol > 0.5) * sin(age * 3.14159265));
        }
    }
    if density < 0.001 && symbols < 0.001 { return vec4f(0.0); }
    let turbulence = fbm(p / s * 0.065 + vec2f(sin(time * 0.7) * params.swirl, time * 0.9 + text.seed));
    let cloud = min(density, 1.0) * (0.25 + turbulence * 0.55) * min(params.intensity, 1.0);
    let tint = mix(params.base, params.bright, turbulence);
    let fog = vec4f(tint.rgb * cloud * tint.a, cloud * tint.a);
    let alpha = symbols * min(params.intensity, 1.0) * params.accent.a * 0.85;
    let particle = vec4f(params.accent.rgb * alpha, alpha);
    return particle + fog * (1.0 - particle.a);
}

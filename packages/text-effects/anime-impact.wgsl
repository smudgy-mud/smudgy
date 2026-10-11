struct Parameters { intensity: f32, speed: f32, base: vec4f, bright: vec4f, accent: vec4f }
@group(1) @binding(0) var<uniform> params: Parameters;
fn hash(p: vec2f) -> f32 { return fract(sin(dot(p, vec2f(127.1, 311.7))) * 43758.5453); }
fn tinted(amount: f32, heat: f32) -> vec4f {
    let tint = mix(mix(params.base, params.bright, clamp(heat, 0.0, 1.0)), params.accent, smoothstep(0.6, 1.0, heat));
    let alpha = clamp(amount * params.intensity, 0.0, 1.0) * tint.a;
    return vec4f(tint.rgb * alpha, alpha);
}

// Three bands of broad luminous rays rotate slowly in opposing directions.
// The central ellipse stays clear so the source text remains readable.
fn effect(p: vec2f) -> vec4f {
    let phase = select(fract(text.time * params.speed / 3.2), text.progress, text.duration > 0.0);
    let s = text.effect_scale;
    let q = (p - text.text_size * 0.5) / s;
    let axes = text.text_size / s * 0.5 + vec2f(30.0, 20.0);
    let ellipse = length(q / axes);
    let radius = length(q);
    let angle = atan2(q.y, q.x);
    let attack = smoothstep(0.0, 0.15, phase);
    let release = 1.0 - smoothstep(0.65, 1.0, phase);
    var bars = 0.0;
    var halo = 0.0;
    for (var layer = 0; layer < 3; layer++) {
        let count = 17.0 + f32(layer) * 9.0;
        let rotation = text.time * params.speed * select(0.075, -0.045, layer == 1) * (1.0 + f32(layer) * 0.4);
        let turn = (angle + rotation + f32(layer) * 0.71 + 3.141593) / 6.283185 * count;
        let lane = floor(turn);
        let random = hash(vec2f(lane, text.seed + f32(layer) * 17.0));
        let width = 0.045 + pow(random, 1.7) * 0.30;
        let angular = abs(fract(turn) - 0.5);
        let aa = max(0.004, count / (radius + 30.0) * 0.12);
        let blade = 1.0 - smoothstep(width - aa, width + aa, angular);
        let inner = 1.05 + hash(vec2f(lane, f32(layer) + 3.0)) * 0.9 + (1.0 - attack) * 5.0;
        let radial = smoothstep(inner, inner + 0.2, ellipse);
        let lengthGate = 1.0 - smoothstep(260.0 + random * 350.0, 440.0 + random * 550.0, radius);
        bars = max(bars, blade * radial * lengthGate * (0.65 + f32(layer) * 0.15));
        halo += exp(-angular / max(width * 1.6, 0.01)) * radial * lengthGate * 0.05;
    }
    let front = 0.7 + phase * 14.0;
    let wave = exp(-abs(ellipse - front) * 10.0) * (1.0 - phase) * 0.5;
    let flash = exp(-pow((phase - 0.15) / 0.065, 2.0)) * 0.08;
    return tinted((bars + halo + wave + flash) * attack * release, clamp(bars + wave, 0.0, 1.0));
}

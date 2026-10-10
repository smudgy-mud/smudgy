struct Parameters { intensity: f32, speed: f32, base: vec4f, bright: vec4f, accent: vec4f }
@group(1) @binding(0) var<uniform> params: Parameters;
fn hash(p: vec2f) -> f32 { return fract(sin(dot(p, vec2f(127.1, 311.7))) * 43758.5453); }
fn tinted(amount: f32, heat: f32) -> vec4f {
    let tint = mix(mix(params.base, params.bright, clamp(heat, 0.0, 1.0)), params.accent, smoothstep(0.6, 1.0, heat));
    let alpha = clamp(amount * params.intensity, 0.0, 1.0) * tint.a;
    return vec4f(tint.rgb * alpha, alpha);
}

fn effect(p: vec2f) -> vec4f {
    let phase = select(fract(text.time * params.speed / 1.4), text.progress, text.duration > 0.0);
    let s = text.effect_scale;
    let q = (p - text.text_size * 0.5) / s;
    let r = length(q);
    let ease = 1.0 - pow(1.0 - phase, 3.0);
    let radius = 12.0 + ease * 210.0;
    let ring = exp(-pow((r - radius) / (2.0 + phase * 5.0), 2.0));
    let inner = exp(-pow((r - radius * 0.7) / 2.0, 2.0)) * 0.45;
    let flash = exp(-r / 45.0) * exp(-phase * 16.0);
    let a = atan2(q.y, q.x);
    let spikes = pow(max(cos(a * 11.0 + text.seed), 0.0), 28.0) * exp(-abs(r - radius * 0.88) / 23.0);
    return tinted((ring + inner + spikes) * (1.0 - phase) + flash, max(ring, flash));
}

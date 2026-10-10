struct Parameters { intensity: f32, speed: f32, base: vec4f, bright: vec4f, accent: vec4f }
@group(1) @binding(0) var<uniform> params: Parameters;
fn hash(p: vec2f) -> f32 { return fract(sin(dot(p, vec2f(127.1, 311.7))) * 43758.5453); }
fn noise(p: vec2f) -> f32 {
    let i = floor(p); let f = fract(p); let t = f * f * (3.0 - 2.0 * f);
    return mix(mix(hash(i), hash(i + vec2f(1, 0)), t.x), mix(hash(i + vec2f(0, 1)), hash(i + vec2f(1, 1)), t.x), t.y);
}
fn fbm(p: vec2f) -> f32 { return noise(p) * 0.57 + noise(p * 2.03 + vec2f(13.1, 7.7)) * 0.28 + noise(p * 4.11 + vec2f(-3.2, 19.3)) * 0.15; }
// The words dissolve into drifting residue, then reassemble before the cycle ends.
fn effect(p: vec2f) -> vec4f {
    if params.intensity <= 0.0 { return sampleText(p); }
    if text.duration > 0.0 && text.progress >= 1.0 { return sampleText(p); }
    let amount = select(0.5 - 0.5 * cos(text.time * params.speed * 1.308997), pow(max(sin(text.progress * 3.14159265), 0.0), 1.25), text.duration > 0.0);
    let s = text.effect_scale;
    let time = text.time * params.speed;
    let grain = fbm(p / s * 0.095 + vec2f(text.seed, time * 0.22));
    let keep = 1.0 - smoothstep(grain - 0.09, grain + 0.09, amount * 1.2 - 0.1);
    let original = sampleText(p) * keep;
    let drift = vec2f(sin(p.y / s * 0.06 + time * 1.8) * 16.0, 42.0) * amount * s;
    var residue = 0.0;
    for (var i = 1; i <= 6; i++) {
        let t = f32(i) / 6.0;
        residue += coverage(p + drift * t + vec2f((grain - 0.5) * 10.0 * amount * s, 0.0)) * (1.0 - t * 0.7);
    }
    let color = mix(mix(params.base, params.bright, grain), params.accent, smoothstep(0.65, 1.0, grain));
    let cloud = min(residue * 0.16 * params.intensity, 0.6) * sin(amount * 3.14159265) * (1.0 - keep * coverage(p)) * color.a;
    return original + vec4f(color.rgb * cloud, cloud) * (1.0 - original.a);
}

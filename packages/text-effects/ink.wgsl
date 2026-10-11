// Continuous ink deformation and wet spreading arrival. Bounded texture reads.
struct Parameters { intensity: f32, speed: f32, variant: f32, amplitude: f32,
    mode: f32, base: vec4f, bright: vec4f, accent: vec4f }
@group(1) @binding(0) var<uniform> params: Parameters;
fn phase() -> f32 {
    return select(fract(text.time * params.speed / 2.4), text.progress, text.duration > 0.0);
}
fn wetness() -> f32 {
    let p = phase();
    if params.mode < 0.5 { return 1.0 - smoothstep(0.0, 0.94, p); }
    if params.mode < 1.5 { return smoothstep(0.03, 0.94, p); }
    return smoothstep(0.03, 0.43, p) * (1.0 - smoothstep(0.57, 0.98, p));
}
fn blot(q: vec2f) -> f32 {
    let cell = floor(q); let f = smoothstep(vec2f(0), vec2f(1), fract(q));
    let a = fract(sin(dot(cell, vec2f(127.1,311.7))) * 43758.5453);
    let b = fract(sin(dot(cell + vec2f(1,0), vec2f(127.1,311.7))) * 43758.5453);
    let c = fract(sin(dot(cell + vec2f(0,1), vec2f(127.1,311.7))) * 43758.5453);
    let d = fract(sin(dot(cell + vec2f(1), vec2f(127.1,311.7))) * 43758.5453);
    return mix(mix(a,b,f.x),mix(c,d,f.x),f.y);
}
fn effect(p: vec2f) -> vec4f {
    if params.intensity * params.amplitude <= 0.0 || params.speed <= 0.0 { return sampleText(p); }
    let d = wetness();
    if d <= 0.0001 { return sampleText(p); }
    let power = min(params.intensity * params.amplitude * text.effect_scale, 2.0);
    if params.variant < 0.5 {
        let top = max(textBaseline() - text.text_size.y * 0.75, 0.0);
        let wobble = 0.45 + 0.35 * sin(p.x * 0.21 + sin(p.x * 0.07) * 2.0);
        let stretch = 1.0 + d * power * wobble * 1.6;
        let source = vec2f(p.x + sin((p.y - top) * 0.22 + p.x * 0.06) * d * power * 1.6,
            top + (p.y - top) / stretch);
        if any(source < vec2f(-2)) || any(source > text.text_size + vec2f(2)) { return vec4f(0); }
        let ink = sampleText(source);
        let shade = 1.0 - d * 0.18 * smoothstep(top, textBaseline() + 6.0, p.y);
        return vec4f(ink.rgb * shade, ink.a);
    }
    if any(p < vec2f(-2)) || any(p > text.text_size + vec2f(2)) { return vec4f(0); }
    let field = blot(p / max(3.0 * text.effect_scale, 1.0)) * 0.65
        + blot(p / max(9.0 * text.effect_scale, 1.0)) * 0.35;
    let reveal = smoothstep(d - 0.10, d + 0.10, field);
    let edge = exp(-pow(abs(field - d) / 0.08, 2.0)) * sin(d * 3.141593);
    var ink = sampleText(p);
    // Bleeding is confined to the capture's two-pixel padding, then dries crisp.
    for (var i = 0u; i < 4u; i++) {
        let angle = f32(i) * 1.570796;
        let neighbour = sampleText(p + vec2f(cos(angle),sin(angle)) * min(edge * power * 1.8, 1.8));
        if neighbour.a > ink.a { ink = neighbour; }
    }
    let tint = edge * 0.28 * power;
    return vec4f(mix(ink.rgb, params.bright.rgb * ink.a, min(tint,0.5)), ink.a) * reveal;
}

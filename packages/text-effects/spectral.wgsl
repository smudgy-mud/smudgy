struct Parameters { intensity: f32, speed: f32, base: vec4f, bright: vec4f, accent: vec4f }
@group(1) @binding(0) var<uniform> params: Parameters;
fn hash(p: vec2f) -> f32 { return fract(sin(dot(p, vec2f(127.1, 311.7))) * 43758.5453); }
fn tinted(amount: f32, heat: f32) -> vec4f {
    let tint = mix(mix(params.base, params.bright, clamp(heat, 0.0, 1.0)), params.accent, smoothstep(0.6, 1.0, heat));
    let alpha = clamp(amount * params.intensity, 0.0, 1.0) * tint.a;
    return vec4f(tint.rgb * alpha, alpha);
}


// Slow pale afterimages float behind the word; the main glyph image stays readable.
fn effect(p: vec2f) -> vec4f {
    if params.intensity <= 0.0 { return sampleText(p); }
    let s = text.effect_scale;
    let time = text.time * params.speed;
    let drift = vec2f(7.0 + sin(time * 0.8) * 5.0, cos(time * 0.6) * 4.0) * s;
    let amount = min(params.intensity, 1.0);
    let original = sampleText(p) * mix(1.0, 0.82 + sin(time * 1.2) * 0.05, amount);
    let near = coverage(p - drift);
    let far = coverage(p + drift * 1.6);
    let wisp = coverage(p - drift - vec2f(s * 2.0, 0.0)) * 0.045
             + coverage(p - drift + vec2f(s * 2.0, 0.0)) * 0.045;
    let color = mix(params.bright, params.accent, 0.45 + 0.35 * sin(time * 0.55 + p.x / s * 0.012));
    let alpha = clamp((near * 0.28 + far * 0.12 + wisp) * amount, 0.0, 0.65) * color.a;
    let ghost = vec4f(color.rgb * alpha, alpha);
    return original + ghost * (1.0 - original.a);
}

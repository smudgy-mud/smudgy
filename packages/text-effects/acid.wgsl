struct Parameters { intensity: f32, speed: f32, base: vec4f, bright: vec4f, accent: vec4f }
@group(1) @binding(0) var<uniform> params: Parameters;
fn hash(p: vec2f) -> f32 { return fract(sin(dot(p, vec2f(127.1, 311.7))) * 43758.5453); }
fn tinted(amount: f32, heat: f32) -> vec4f {
    let tint = mix(mix(params.base, params.bright, clamp(heat, 0.0, 1.0)), params.accent, smoothstep(0.6, 1.0, heat));
    let alpha = clamp(amount * params.intensity, 0.0, 1.0) * tint.a;
    return vec4f(tint.rgb * alpha, alpha);
}


// Wet glyph edges feed pendant drops; narrow rivulets fall and break into beads.
fn effect(p: vec2f) -> vec4f {
    let s = text.effect_scale;
    let time = text.time * params.speed;
    var drops = 0.0;
    var shine = 0.0;
    for (var i = 0; i < 22; i++) {
        let id = f32(i) + text.seed;
        let clock = time * (0.32 + hash(vec2f(id, 1.0)) * 0.13) + hash(vec2f(id, 2.0));
        let age = fract(clock);
        let x = (f32(i) + 0.2 + hash(vec2f(id, 4.0)) * 0.6) / 22.0 * text.text_size.x;
        let origin = vec2f(x, text.text_size.y * 0.64);
        let q = (p - origin) / s;
        if abs(q.x) > 7.0 || q.y < -7.0 || q.y > 32.0 { continue; }
        let ink = max(coverage(origin), max(coverage(origin + vec2f(2.0, 0.0)), coverage(origin - vec2f(2.0, 0.0))));
        if ink < 0.05 { continue; }
        let fall = smoothstep(0.30, 1.0, age);
        let y = 1.5 + fall * fall * 26.0;
        let size = 1.5 + sin(age * 3.141593) * 2.0;
        let v = (q - vec2f(0.0, y)) / vec2f(size, size * (1.0 + fall * 0.5));
        let bead = 1.0 - smoothstep(0.70, 1.1, length(v));
        let thread = exp(-abs(q.x) * 1.8) * smoothstep(-1.0, 0.0, q.y) * (1.0 - smoothstep(y - 1.0, y + 0.8, q.y)) * (1.0 - smoothstep(0.45, 0.8, age));
        let glint = exp(-dot(v + vec2f(0.32, 0.32), v + vec2f(0.32, 0.32)) * 15.0);
        let fade = (1.0 - smoothstep(0.80, 1.0, age)) * ink;
        drops += (bead * 0.8 + thread * 0.55) * fade;
        shine += glint * fade;
    }
    var wet = 0.0;
    if p.x > -4.0 * s && p.x < text.text_size.x + 4.0 * s && p.y > -4.0 * s && p.y < text.text_size.y + 4.0 * s {
        let ink = coverage(p);
        let rim = max(max(coverage(p + vec2f(1.8 * s, 0.0)), coverage(p - vec2f(1.8 * s, 0.0))), max(coverage(p + vec2f(0.0, 1.8 * s)), coverage(p - vec2f(0.0, 1.8 * s)))) * (1.0 - ink);
        wet = rim * (0.35 + 0.12 * sin(p.x / s * 0.11 - time * 0.55));
    }
    return tinted(drops + wet + shine * 0.45, max(0.55, shine + drops * 0.65 + wet * 0.3));
}

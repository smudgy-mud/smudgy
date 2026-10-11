// Healing travels through the word; blessing descends and seals into a gold underline.
struct Parameters { intensity: f32, speed: f32, variant: f32, base: vec4f, bright: vec4f, accent: vec4f }
@group(1) @binding(0) var<uniform> params: Parameters;
fn hash(n: f32) -> f32 { return fract(sin(n * 127.1 + 311.7) * 43758.5453); }
fn tinted(amount: f32, heat: f32) -> vec4f {
    var tint = mix(mix(params.base, params.bright, clamp(heat, 0.0, 1.0)), params.accent, smoothstep(0.7, 1.0, heat));
    let light = smoothstep(0.35, 0.8, dot(text.background.rgb, vec3f(0.2126, 0.7152, 0.0722)));
    tint = vec4f(mix(tint.rgb, mix(params.base.rgb, params.bright.rgb, 0.2), light * 0.65), tint.a);
    let alpha = clamp(amount * params.intensity, 0.0, 1.0) * tint.a;
    return vec4f(tint.rgb * alpha, alpha);
}
fn star(p: vec2f, size: f32) -> f32 {
    return exp(-abs(p.x) * 2.0) * exp(-abs(p.y) / size) + exp(-abs(p.y) * 2.0) * exp(-abs(p.x) / size);
}
fn effect(p: vec2f) -> vec4f {
    let phase = select(fract(text.time * params.speed / 2.4), text.progress, text.duration > 0.0);
    let s = text.effect_scale;
    let q = vec2f(p.x / s, (p.y - text.text_size.y * 0.5) / s);
    let width = text.text_size.x / s;
    let height = text.text_size.y / s;
    var shape = 0.0;
    var shine = 0.0;
    if params.variant < 0.5 {
        let front = mix(-25.0, width + 35.0, smoothstep(0.04, 0.9, phase));
        let waveX = front - q.y * q.y / max(height * 3.0, 50.0);
        let d = q.x - waveX;
        let vertical = exp(-pow(q.y / (height * 0.5 + 9.0), 4.0));
        shape = exp(-d * d / 100.0) * vertical * 0.48;
        shine = exp(-d * d / 2.3) * vertical;
        var rim = 0.0;
        for (var i = 0; i < 4; i++) {
            let a = f32(i) * 1.570796;
            rim = max(rim, coverage(p + vec2f(cos(a), sin(a)) * 1.5 * s));
        }
        shape += max(rim - coverage(p), 0.0) * exp(-d * d / 420.0) * 0.85;
        for (var i = 0; i < 10; i++) {
            let id = f32(i) + text.seed;
            let x = hash(id) * width;
            let birth = 0.08 + x / max(width, 1.0) * 0.65;
            let age = clamp((phase - birth) / 0.3, 0.0, 1.0);
            let point = vec2f(x + sin(age * 2.8 + id) * 2.0, -height * 0.45 - age * 15.0);
            let alive = sin(age * 3.141593);
            let r = q - point;
            shine += star(r, 3.0) * alive * 0.5;
            shape += exp(-dot(r, r) / 15.0) * alive * 0.13;
        }
    } else {
        let descent = mix(-height * 0.5 - 25.0, height * 0.5 + 3.5, smoothstep(0.03, 0.58, phase));
        let xGate = smoothstep(-8.0, 8.0, q.x) * (1.0 - smoothstep(width - 8.0, width + 8.0, q.x));
        let settling = 1.0 - smoothstep(0.52, 0.78, phase);
        let thread = exp(-pow((q.y - descent) / 1.1, 2.0));
        shine = thread * xGate * settling * 0.7;
        shape = exp(-pow((q.y - descent) / 7.0, 2.0)) * xGate * settling * 0.2;
        let seal = smoothstep(0.48, 0.70, phase);
        let halfLine = width * 0.5 * smoothstep(0.48, 0.80, phase);
        let underline = exp(-pow((q.y - height * 0.5 - 3.5) / 0.8, 2.0)) * (1.0 - smoothstep(halfLine - 4.0, halfLine + 2.0, abs(q.x - width * 0.5)));
        shine += underline * seal * 0.75;
        for (var i = 0; i < 7; i++) {
            let id = f32(i) + text.seed;
            let x = width * (f32(i) + 0.5) / 7.0;
            let y = descent - 6.0 - hash(id) * 10.0;
            let pulse = pow(max(sin(phase * 3.141593 + hash(id) * 1.2), 0.0), 3.0);
            shine += star(q - vec2f(x, y), 4.0) * pulse * settling * 0.6;
        }
        let centre = q - vec2f(width * 0.5, height * 0.5 + 3.5);
        shine += star(centre, 8.0) * seal * exp(-max(phase - 0.72, 0.0) * 7.0) * 0.65;
    }
    return tinted(shape + shine, clamp(shine, 0.0, 1.0));
}

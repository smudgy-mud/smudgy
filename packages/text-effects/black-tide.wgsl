// Joined ink-black pools consume lettering behind a visibly travelling liquid edge.
struct Parameters { intensity: f32, speed: f32, base: vec4f, bright: vec4f, accent: vec4f }
@group(1) @binding(0) var<uniform> params: Parameters;
fn over(a: vec4f, b: vec4f) -> vec4f { return a + b * (1.0 - a.a); }
fn paint(color: vec4f, alpha: f32) -> vec4f {
    let a = clamp(alpha, 0.0, 1.0) * color.a;
    return vec4f(color.rgb * a, a);
}
fn effect(p: vec2f) -> vec4f {
    let original = sampleText(p);
    if params.intensity <= 0.0 || params.speed <= 0.0 { return original; }
    let phase = select(fract(text.time * params.speed / 3.6), text.progress, text.duration > 0.0);
    let rise = smoothstep(0.03, 0.55, phase);
    let retreat = smoothstep(0.62, 0.98, phase);
    let tide = rise * (1.0 - retreat);
    if tide <= 0.0001 { return original; }
    let s = text.effect_scale;
    let h = text.text_size.y;
    let center = textBaseline() - h * 0.33;
    if p.x < -26.0 * s || p.x > text.text_size.x + 26.0 * s
        || abs(p.y - center) > h * 0.48 + 18.0 * s { return original; }
    let clock = text.time * params.speed;
    let q = vec3f(p.x / (34.0 * s), p.y / (18.0 * s), clock * 0.16 + text.seed * 0.01);
    let n = noise3D(q, 0.0) * 0.7 + noise3D(q * 2.3 + vec3f(11.0), 0.0) * 0.3;
    let moving = mix(-24.0 * s, text.text_size.x + 24.0 * s, tide);
    let edge = moving + (sin((p.y - center) / (5.0 * s) + clock * 1.2) * 4.0 + n * 7.0) * s;
    let reach = 1.0 - smoothstep(edge - 1.4 * s, edge + 1.4 * s, p.x);
    let trailing = smoothstep(-23.0 * s, -9.0 * s, p.x);
    let spacing = max(h * 0.78, 14.0) * s;
    let node = floor(p.x / spacing);
    var distance = 1000.0;
    // Nearby pools thicken and join behind the moving front. Their scalloped
    // silhouette is a body of liquid, not a rectangular cover over the word.
    for (var i = -1; i <= 1; i++) {
        let at = node + f32(i);
        let seed = fract(sin(at * 127.1 + text.seed) * 43758.5453);
        let x = (at + 0.5) * spacing;
        let age = smoothstep(0.0, spacing * 2.0, edge - x);
        let origin = vec2f(x, center + (seed - 0.5) * 7.0 * s);
        let radius = vec2f(spacing * (0.59 + age * 0.24), h * (0.34 + age * 0.13) + seed * 3.0 * s);
        let d = (length((p - origin) / radius) - 1.0) * min(radius.x, radius.y);
        distance = min(distance, d);
    }
    let surface = (1.0 - smoothstep(-0.7, 1.1, distance)) * reach * trailing;
    let crest = exp(-pow(abs(p.x - edge) / (1.7 * s), 2.0))
        * (1.0 - smoothstep(-1.0, 1.5, distance));
    let shore = exp(-pow(abs(distance) / (1.05 * s), 2.0)) * reach * trailing;
    let light = smoothstep(0.35, 0.80, dot(text.background.rgb, vec3f(0.2126,0.7152,0.0722)));
    let dark = mix(params.base.rgb, vec3f(0.012,0.015,0.022), light * 0.8);
    let reflect = (shore * 0.65 + crest * 0.75) * (0.55 + 0.45 * sin(p.x / (9.0 * s) + n * 3.0));
    var pool = paint(vec4f(mix(dark, params.bright.rgb, max(reflect, 0.0)), params.base.a), surface);
    pool = over(paint(params.accent, crest * 0.23), pool);
    // Ink beside the front is pulled into it, then replaced by opaque darkness.
    let tug = sin((p.y-center) / (4.0*s)) * crest * 3.0*s;
    let ink = sampleText(p + vec2f(tug, 0.0));
    return mix(original, over(pool, ink), min(params.intensity, 1.0));
}

// Ten contextual signals share one compiled program and bounded analytical geometry.
struct Parameters { intensity: f32, speed: f32, variant: f32, base: vec4f, bright: vec4f, accent: vec4f }
@group(1) @binding(0) var<uniform> params: Parameters;
fn hash(p: vec2f) -> f32 { return fract(sin(dot(p, vec2f(127.1, 311.7))) * 43758.5453); }
fn line(p: vec2f, a: vec2f, b: vec2f, width: f32) -> f32 {
    let v = b - a;
    let t = clamp(dot(p - a, v) / max(dot(v, v), 0.001), 0.0, 1.0);
    return 1.0 - smoothstep(width, width + 1.2, length(p - a - t * v));
}
fn heart(p: vec2f) -> f32 {
    let q = vec2f(p.x, -p.y);
    let a = dot(q, q) - 1.0;
    return 1.0 - smoothstep(-0.07, 0.08, a * a * a - q.x * q.x * q.y * q.y * q.y);
}
fn sparkle(p: vec2f, radius: f32) -> f32 {
    return exp(-abs(p.x) * 1.5) * exp(-abs(p.y) / radius)
         + exp(-abs(p.y) * 1.5) * exp(-abs(p.x) / radius);
}
fn box(p: vec2f, halfsize: vec2f, radius: f32) -> f32 {
    let v = abs(p) - halfsize + vec2f(radius);
    return length(max(v, vec2f(0.0))) + min(max(v.x, v.y), 0.0) - radius;
}
fn effect(p: vec2f) -> vec4f {
    if params.intensity <= 0.0 { return vec4f(0.0); }
    let t = select(fract(text.time * params.speed / 2.4), text.progress, text.duration > 0.0);
    let ease = 1.0 - pow(1.0 - t, 3.0);
    let s = text.effect_scale;
    let q = (p - text.text_size * 0.5) / s;
    let r = length(q);
    let angle = atan2(q.y, q.x);
    var body = 0.0;
    var accent = 0.0;
    var mist = 0.0;
    if params.variant < 0.5 {
        // A close red outline launches an expanding wave; the source has no dot.
        let radius = 4.0 + (1.0 - pow(1.0 - t, 2.0)) * 4092.0;
        let halfsize = text.text_size / s * 0.5 + vec2f(4.0, 1.0);
        let border = abs(box(q, halfsize, 3.0));
        accent = exp(-border * 2.0) * (1.0 - smoothstep(0.35, 0.85, t));
        body = exp(-pow((r - radius) / 3.0, 2.0)) * (1.0 - t);
        mist = (1.0 - smoothstep(radius - 2.0, radius, r)) * 0.045 * (1.0 - t);
    } else if params.variant < 1.5 {
        // Greeting: two friendly ripples, with a small heart above the name.
        let radius = 12.0 + ease * 420.0;
        body = (exp(-abs(r - radius) / 3.5) + exp(-abs(r - radius * 0.72) / 2.5) * 0.55) * (1.0 - t);
        accent = heart((q + vec2f(0.0, text.text_size.y / s * 0.5 + 15.0)) / 7.0) * sin(t * 3.141593);
    } else if params.variant < 2.5 {
        // Compact closing end brackets and a hot underline fit the line's height.
        let x = text.text_size.x / s * 0.5 + 6.0 + (1.0 - ease) * 24.0;
        let y = max(text.text_size.y / s * 0.5 - 2.0, 3.0);
        let v = vec2f(abs(q.x), abs(q.y));
        body = line(v, vec2f(x + 4.0, y), vec2f(x, y), 1.2)
             + line(v, vec2f(x, 0.0), vec2f(x, y), 1.2);
        accent = exp(-abs(q.y - y) / 0.9) * (1.0 - smoothstep(x - 6.0, x, abs(q.x))) * sin(t * 3.141593) * 0.55;
        body *= 1.0 - smoothstep(0.65, 1.0, t);
    } else if params.variant < 3.5 {
        // Dread: a slow serrated halo without an additional warning symbol.
        let radius = 30.0 + ease * 330.0;
        let tooth = abs(fract((angle + 3.141593) * 3.0 + text.seed) - 0.5) * 2.0;
        let jagged = r + tooth * 28.0;
        body = exp(-abs(jagged - radius) / 5.0) * (1.0 - t);
        mist = exp(-pow((r - radius) / 45.0, 2.0)) * 0.18 * (1.0 - t);

    } else if params.variant < 4.5 {
        // Quest: a hovering diamond marker and a narrow upward beacon.
        let v = q + vec2f(0.0, text.text_size.y / s * 0.5 + 23.0 + sin(t * 3.141593) * 7.0);
        accent = exp(-abs(abs(v.x) + abs(v.y) - 10.0) / 1.5);
        body = exp(-abs(q.x) / 8.0) * smoothstep(0.0, 20.0, -q.y) * exp(-max(-q.y, 0.0) / 180.0) * sin(t * 3.141593);
        mist = exp(-abs(q.x) / 25.0) * exp(-abs(q.y) / 180.0) * 0.10;
    } else if params.variant < 5.5 {
        // Level up: rising rays and a fountain of gold stars.
        for (var i = 0; i < 16; i++) {
            let id = f32(i) + text.seed;
            let x = (hash(vec2f(id, 1.0)) - 0.5) * (text.text_size.x / s + 180.0);
            let age = clamp((t - hash(vec2f(id, 2.0)) * 0.25) / 0.75, 0.0, 1.0);
            let center = vec2f(x * (0.5 + age), 20.0 - age * (100.0 + hash(vec2f(id, 4.0)) * 200.0));
            let v = q - center;
            if abs(v.x) < 30.0 && abs(v.y) < 40.0 {
                accent += sparkle(v, 5.0) * sin(age * 3.141593);
            }
            body += exp(-abs(q.x - x) / 2.0) * exp(-abs(q.y + age * 130.0) / 55.0) * sin(age * 3.141593) * 0.10;
        }
    } else if params.variant < 6.5 {
        // Heal needed: a heart and two rings drawing attention inward.
        let beat = 0.7 + 0.3 * pow(max(sin(t * 12.56637), 0.0), 2.0);
        let v = q + vec2f(0.0, text.text_size.y / s * 0.5 + 18.0);
        accent = heart(v / (9.0 * beat));
        let radius = 16.0 + (1.0 - ease) * 320.0;
        body = (exp(-abs(r - radius) / 3.0) + exp(-abs(r - radius * 1.4) / 2.0) * 0.5) * sin(t * 3.141593);
    } else if params.variant < 7.5 {
        // Expiring: contracting broken ring, hourglass and falling grains.
        let radius = 16.0 + (1.0 - t) * 110.0;
        body = exp(-abs(r - radius) / 2.0) * smoothstep(-0.2, 0.4, sin(angle * 12.0 + t * 6.0)) * (1.0 - t);
        let v = q + vec2f(0.0, text.text_size.y / s * 0.5 + 20.0);
        accent = line(v, vec2f(-7,-10), vec2f(7,10), 1.0) + line(v, vec2f(7,-10), vec2f(-7,10), 1.0)
               + line(v, vec2f(-8,-10), vec2f(8,-10), 1.0) + line(v, vec2f(-8,10), vec2f(8,10), 1.0);
        accent *= 1.0 - t;
        for (var i = 0; i < 8; i++) {
            let id = f32(i) + text.seed;
            let center = vec2f((hash(vec2f(id, 1.0)) - 0.5) * text.text_size.x / s, t * (40.0 + hash(vec2f(id, 2.0)) * 80.0));
            accent += exp(-dot(q - center, q - center) / 2.0) * (1.0 - t);
        }
    } else if params.variant < 8.5 {
        // Rally: three rising chevrons and an outward supporting wave.
        for (var i = 0; i < 3; i++) {
            let v = q + vec2f(0.0, text.text_size.y / s * 0.5 + 14.0 + f32(i) * 12.0 + ease * 35.0);
            accent += exp(-abs(v.y - abs(v.x) * 0.5) / 1.6) * (1.0 - smoothstep(16.0, 23.0, abs(v.x))) * sin(t * 3.141593);
        }
        body = exp(-abs(r - (20.0 + ease * 250.0)) / 4.0) * (1.0 - t) * 0.5;
    } else {
        // Discovery: a compass star and widely spaced twinkling sparks.
        accent = sparkle(q, 45.0) * pow(sin(t * 3.141593), 2.0);
        let radius = 15.0 + ease * 230.0;
        for (var i = 0; i < 8; i++) {
            let a = f32(i) * 0.785398 + text.seed;
            let v = q - vec2f(cos(a), sin(a)) * radius;
            if abs(v.x) < 45.0 && abs(v.y) < 45.0 {
                body += sparkle(v, 8.0) * sin(t * 3.141593);
            }
        }
    }
    let alpha = clamp((body * 0.75 + accent + mist) * params.intensity, 0.0, 1.0);
    let tint = mix(mix(params.base, params.bright, clamp(body, 0.0, 1.0)), params.accent, clamp(accent, 0.0, 1.0));
    return vec4f(tint.rgb * alpha * tint.a, alpha * tint.a);
}

// Five short combat gestures share timing, segment geometry and spark trajectories.
struct Parameters { intensity: f32, speed: f32, variant: f32, base: vec4f, bright: vec4f, accent: vec4f }
@group(1) @binding(0) var<uniform> params: Parameters;
fn hash(n: f32) -> f32 { return fract(sin(n * 127.1 + 311.7) * 43758.5453); }
fn segment(p: vec2f, a: vec2f, b: vec2f) -> f32 {
    let v = b - a;
    return length(p - a - v * clamp(dot(p - a, v) / max(dot(v, v), 0.001), 0.0, 1.0));
}
fn tinted(amount: f32, heat: f32) -> vec4f {
    var tint = mix(mix(params.base, params.bright, clamp(heat, 0.0, 1.0)), params.accent, smoothstep(0.7, 1.0, heat));
    // Keep the solid gesture visible on light terminal palettes, while retaining its glow.
    let light = smoothstep(0.35, 0.8, dot(text.background.rgb, vec3f(0.2126, 0.7152, 0.0722)));
    tint = vec4f(mix(tint.rgb, mix(params.base.rgb, params.bright.rgb, 0.25), light * 0.65), tint.a);
    let alpha = clamp(amount * params.intensity, 0.0, 1.0) * tint.a;
    return vec4f(tint.rgb * alpha, alpha);
}
fn effect(p: vec2f) -> vec4f {
    let phase = select(fract(text.time * params.speed / 1.5), text.progress, text.duration > 0.0);
    let s = text.effect_scale;
    let q = (p - text.text_size * 0.5) / s;
    let width = text.text_size.x / s;
    let height = text.text_size.y / s;
    var ink = 0.0;
    var glow = 0.0;
    if params.variant < 0.5 {
        // A narrow blade tip crosses the word, leaving three staggered fading cuts.
        let sweep = smoothstep(0.05, 0.58, phase);
        let head = mix(-width * 0.5 - 26.0, width * 0.5 + 38.0, sweep);
        let tail = head - min(width * 0.55, 105.0);
        let slope = -min(height * 0.5, 13.0) / max(width * 0.5, 30.0);
        let attack = smoothstep(0.02, 0.12, phase) * (1.0 - smoothstep(0.58, 0.9, phase));
        let d = segment(q, vec2f(tail, tail * slope), vec2f(head, head * slope));
        ink = exp(-d * d / 1.3) * attack;
        glow = exp(-d * d / 32.0) * attack * 0.32;
        for (var i = 0; i < 3; i++) {
            let lag = f32(i + 1) * 10.0;
            let y = f32(i + 1) * 3.0;
            let trail = segment(q, vec2f(tail - lag, tail * slope + y), vec2f(head - lag * 2.0, head * slope + y));
            ink += exp(-trail * trail / 0.65) * attack * (0.20 - f32(i) * 0.045);
        }
        let tip = length(q - vec2f(head, head * slope));
        ink += exp(-tip * tip / 12.0) * attack;
    } else if params.variant < 1.5 {
        // A decisive four-point flare fractures into independent angular shards.
        let age = clamp((phase - 0.12) / 0.78, 0.0, 1.0);
        let flash = smoothstep(0.04, 0.13, phase) * exp(-age * 9.0);
        let star = pow(abs(q.x) / 26.0, 0.55) + pow(abs(q.y) / 40.0, 0.55);
        ink = (1.0 - smoothstep(0.9, 1.04, star)) * flash;
        glow = exp(-length(q) / 22.0) * flash * 0.55;
        for (var i = 0; i < 12; i++) {
            let id = f32(i) + text.seed;
            let angle = f32(i) * 0.523599 + (hash(id) - 0.5) * 0.3;
            let dir = vec2f(cos(angle), sin(angle));
            let travel = (9.0 + (1.0 - pow(1.0 - age, 2.0)) * (22.0 + hash(id + 2.0) * 43.0));
            let centre = dir * travel * vec2f(1.0, 0.6);
            let r = q - centre;
            let along = dot(r, dir);
            let across = dot(r, vec2f(-dir.y, dir.x));
            let diamond = abs(along) / (4.0 + hash(id + 3.0) * 4.0) + abs(across) / 1.5;
            let shard = (1.0 - smoothstep(0.8, 1.1, diamond)) * sin(age * 3.141593) * (1.0 - age);
            ink += shard;
            glow += exp(-dot(r, r) / 28.0) * shard * 0.22;
        }
    } else if params.variant < 2.5 {
        // Two metal edges meet once; sparks fan out from their point of contact.
        let approach = 1.0 - smoothstep(0.03, 0.3, phase);
        let separation = approach * min(width * 0.5 + 12.0, 150.0);
        let edges = (1.0 - smoothstep(0.32, 0.54, phase));
        let a = segment(q, vec2f(-separation - 30.0, -10.0), vec2f(-separation, 1.0));
        let b = segment(q, vec2f(separation + 30.0, -10.0), vec2f(separation, 1.0));
        ink = (exp(-a * a / 1.1) + exp(-b * b / 1.1)) * edges;
        glow = (exp(-a * a / 24.0) + exp(-b * b / 24.0)) * edges * 0.25;
        let age = clamp((phase - 0.28) / 0.66, 0.0, 1.0);
        let flash = smoothstep(0.25, 0.29, phase) * exp(-age * 14.0);
        ink += (exp(-q.x * q.x / 2.0) * exp(-abs(q.y) / 22.0) + exp(-q.y * q.y / 2.0) * exp(-abs(q.x) / 28.0)) * flash;
        for (var i = 0; i < 14; i++) {
            let id = f32(i) + text.seed;
            let angle = hash(id + 9.0) * 6.283185;
            let velocity = vec2f(cos(angle), sin(angle) * 0.6) * (25.0 + hash(id + 4.0) * 48.0);
            let point = velocity * age + vec2f(0.0, age * age * 22.0);
            let d = segment(q, point, point - velocity * 0.08);
            let alive = smoothstep(0.0, 0.04, age) * pow(1.0 - age, 1.5);
            ink += exp(-d * d / 0.8) * alive;
            glow += exp(-d * d / 14.0) * alive * 0.18;
        }
     } else if params.variant < 3.5 {
        // A broad crescent sweeps clockwise, with a tapered blade and separate wake.
        let radius = vec2f(width * 0.52 + 12.0, height * 0.55 + 22.0);
        let v = (q - vec2f(0.0, 5.0)) / radius;
        let angle = atan2(v.y, v.x);
        let head = mix(-3.0, 0.65, smoothstep(0.04, 0.70, phase));
        let behind = head - angle;
        let gate = smoothstep(0.0, 0.10, behind) * (1.0 - smoothstep(0.55, 1.45, behind));
        let widthHere = 1.0 + 3.0 * (1.0 - clamp(behind / 1.45, 0.0, 1.0));
        let d = abs(length(v) - 1.0) * min(radius.x, radius.y);
        let alive = smoothstep(0.01, 0.10, phase) * (1.0 - smoothstep(0.72, 1.0, phase));
        ink = exp(-d*d / (widthHere*widthHere)) * gate * alive;
        glow = exp(-d*d / 80.0) * gate * alive * 0.24;
        let wake = abs(length(v + vec2f(0.015, -0.1)) - 1.0) * min(radius.x, radius.y);
        ink += exp(-wake*wake / 0.7) * gate * alive * 0.32;
        let tip = q - vec2f(0.0, 5.0) - radius * vec2f(cos(head), sin(head));
        ink += exp(-dot(tip,tip) / 13.0) * alive;
    } else {
        // A compact guard recoils, then a needle-fast counterattack crosses the word.
        let guard = smoothstep(0.0, 0.05, phase) * (1.0 - smoothstep(0.22, 0.34, phase));
        let ring = abs(length((q + vec2f(width * 0.36, 0.0)) / vec2f(10.0, 15.0)) - 1.0) * 10.0;
        ink = exp(-ring*ring / 0.8) * guard * smoothstep(-width * 0.36, -width * 0.30, -q.x);
        let age = smoothstep(0.24, 0.58, phase);
        let tip = mix(-width * 0.5 - 12.0, width * 0.5 + 18.0, age);
        let tail = tip - 28.0 - age * width * 0.65;
        let attack = smoothstep(0.24, 0.30, phase) * (1.0 - smoothstep(0.60, 0.88, phase));
        let d = segment(q, vec2f(tail, 3.0), vec2f(tip, -3.0));
        ink += exp(-d*d / 0.65) * attack;
        glow = exp(-d*d / 24.0) * attack * 0.3;
        let r = q - vec2f(width * 0.5 + 18.0, -3.0);
        let flash = smoothstep(0.54, 0.60, phase) * (1.0 - smoothstep(0.66, 0.90, phase));
        let star = pow(abs(r.x) / 15.0, 0.55) + pow(abs(r.y) / 20.0, 0.55);
        ink += (1.0 - smoothstep(0.8, 1.1, star)) * flash;
    }
    return tinted(ink + glow, clamp(ink, 0.0, 1.0));
}

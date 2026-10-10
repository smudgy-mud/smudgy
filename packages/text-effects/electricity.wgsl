struct Parameters { intensity: f32, speed: f32, base: vec4f, bright: vec4f, accent: vec4f }
@group(1) @binding(0) var<uniform> params: Parameters;
fn hash(p: vec2f) -> f32 { return fract(sin(dot(p, vec2f(127.1, 311.7))) * 43758.5453); }
fn tinted(amount: f32, heat: f32) -> vec4f {
    let tint = mix(mix(params.base, params.bright, clamp(heat, 0.0, 1.0)), params.accent, smoothstep(0.6, 1.0, heat));
    let alpha = clamp(amount * params.intensity, 0.0, 1.0) * tint.a;
    return vec4f(tint.rgb * alpha, alpha);
}

// Bridges use adjacent shaped glyphs, including proportional fonts and ligatures.
fn anchor(ink: vec4f, right: bool) -> vec3f {
    var best = vec3f(ink.xy + ink.zw * 0.5, 0.0);
    for (var x = 0; x < 4; x++) {
        for (var y = 0; y < 4; y++) {
            let point = ink.xy + ink.zw * vec2f(0.10 + f32(x) * 0.267, 0.10 + f32(y) * 0.267);
            let edge = select(1.0 - f32(x) * 0.15, 0.55 + f32(x) * 0.15, right);
            let weight = coverage(point) * edge;
            if weight > best.z { best = vec3f(point, weight); }
        }
    }
    return best;
}
fn effect(p: vec2f) -> vec4f {
    let s = text.effect_scale;
    let clock = text.time * params.speed * 9.0;
    if glyphCount() < 2u { return vec4f(0.0); }
    let count = f32(glyphCount() - 1u);
    var arcs = 0.0;
    var cores = 0.0;
    for (var i = 0; i < 8; i++) {
        // Stagger the bridges so the word stays charged between individual flashes.
        let localClock = clock + f32(i) * 0.618034;
        let tick = floor(localClock);
        let phase = fract(localClock);
        let id = tick + f32(i) * 37.0 + text.seed;
        let cell = u32(floor(fract(hash(vec2f(tick + text.seed, 2.0)) + f32(i) / 8.0) * count));
        let leftGlyph = glyphAt(cell);
        let rightGlyph = glyphAt(cell + 1u);
        // Combining glyphs within one source cluster do not form another letter pair.
        if all(leftGlyph.cluster.xy == rightGlyph.cluster.xy) { continue; }
        let left = leftGlyph.ink;
        let right = rightGlyph.ink;
        // A space is a real shaped glyph with no ink; never jump across it.
        if min(left.z, right.z) <= 0.0 || abs(left.y - right.y) > text.text_size.y * 0.6 { continue; }
        if p.x < left.x - 4.0 * s || p.x > right.x + right.z + 4.0 * s || p.y < -6.0 * s || p.y > text.text_size.y + 6.0 * s { continue; }
        let a = anchor(left, true);
        let b = anchor(right, false);
        if min(a.z, b.z) < 0.1 { continue; }
        let u = clamp((p.x - a.x) / max(b.x - a.x, 2.0), 0.0, 1.0);
        let tooth = hash(vec2f(floor(u * 6.0), id)) * 2.0 - 1.0;
        let y = mix(a.y, b.y, u) + sin(u * 3.141593) * (tooth * 4.0 - 2.0) * s;
        let d = abs(p.y - y) / s;
        let gate = smoothstep(a.x - s, a.x + s, p.x) * (1.0 - smoothstep(b.x - s, b.x + s, p.x));
        let pulse = pow(max(sin(phase * 3.141593), 0.0), 0.55);
        let core = exp(-d * 2.1) * gate * pulse;
        let corona = exp(-d * 0.55) * gate * pulse * 0.23;
        let points = (exp(-length(p - a.xy) / (2.0 * s)) + exp(-length(p - b.xy) / (2.0 * s))) * pulse * 0.65;
        arcs += core + corona + points;
        cores += core + points;
    }
    return tinted(arcs, cores);
}

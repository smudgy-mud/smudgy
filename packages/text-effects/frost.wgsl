struct Parameters { intensity: f32, speed: f32, base: vec4f, bright: vec4f, accent: vec4f }
@group(1) @binding(0) var<uniform> params: Parameters;
fn hash(p: vec2f) -> f32 { return fract(sin(dot(p, vec2f(127.1, 311.7))) * 43758.5453); }
fn tinted(amount: f32, heat: f32) -> vec4f {
    let tint = mix(mix(params.base, params.bright, clamp(heat, 0.0, 1.0)), params.accent, smoothstep(0.6, 1.0, heat));
    let alpha = clamp(amount * params.intensity, 0.0, 1.0) * tint.a;
    return vec4f(tint.rgb * alpha, alpha);
}

fn segment(p: vec2f, a: vec2f, b: vec2f, width: f32) -> f32 {
    let v = b - a;
    let u = clamp(dot(p - a, v) / max(dot(v, v), 0.001), 0.0, 1.0);
    return 1.0 - smoothstep(width, width + 0.65, length(p - a - u * v));
}
// Seeded crystals grow from fixed ink anchors; neither roots nor branches drift.
fn effect(p: vec2f) -> vec4f {
    let s = text.effect_scale;
    let formation = select(clamp(text.time * params.speed / 1.6, 0.0, 1.0), smoothstep(0.0, 0.7, text.progress), text.duration > 0.0);
    var crystals = 0.0;
    var facets = 0.0;
    for (var i = 0; i < 24; i++) {
        let id = f32(i) + text.seed;
        if glyphCount() == 0u { break; }
        let glyph = glyphAt(min(u32(f32(i) / 24.0 * f32(glyphCount())), glyphCount() - 1u)).ink;
        if glyph.z <= 0.0 { continue; }
        var origin = glyph.xy + glyph.zw * vec2f(0.18 + hash(vec2f(id, 1.0)) * 0.64, 0.55);
        // Reject pixels before sampling possible roots inside this glyph's actual ink.
        if abs(p.x - origin.x) > 14.0 * s || abs(p.y - origin.y) > 26.0 * s { continue; }
        var ink = 0.0;
        for (var j = 0; j < 6; j++) {
            let candidate = glyph.xy + glyph.zw * vec2f((origin.x - glyph.x) / max(glyph.z, 1.0), 0.15 + f32(j) * 0.14);
            let coverageHere = coverage(candidate);
            if coverageHere > ink { ink = coverageHere; origin = candidate; }
        }
        let q = (p - origin) / s;
        if abs(q.x) > 12.0 || abs(q.y) > 24.0 { continue; }

        let grow = smoothstep(hash(vec2f(id, 3.0)) * 0.48, 0.65 + hash(vec2f(id, 3.0)) * 0.35, formation);
        if grow <= 0.001 || ink < 0.05 { continue; }
        let height = (7.0 + hash(vec2f(id, 4.0)) * 13.0) * grow;
        let angle = (hash(vec2f(id, 5.0)) - 0.5) * 0.7;
        let v = vec2f(q.x * cos(angle) - q.y * sin(angle), q.x * sin(angle) + q.y * cos(angle));
        var crystal = segment(v, vec2f(0.0), vec2f(0.0, -height), 0.42);
        for (var j = 1; j < 4; j++) {
            let h = f32(j) * height / 4.0;
            let reach = (height - h) * 0.43;
            crystal = max(crystal, segment(v, vec2f(0.0, -h), vec2f(reach, -h - reach * 0.8), 0.28));
            crystal = max(crystal, segment(v, vec2f(0.0, -h), vec2f(-reach, -h - reach * 0.8), 0.28));
        }
        crystals = max(crystals, crystal * ink);
        facets = max(facets, exp(-abs(v.x) * 0.75) * (1.0 - smoothstep(0.0, 1.5, max(v.y, -height - v.y))) * ink * 0.12);
    }
    let edge = max(max(coverage(p + vec2f(s, 0.0)), coverage(p - vec2f(s, 0.0))), max(coverage(p + vec2f(0.0, s)), coverage(p - vec2f(0.0, s)))) * (1.0 - coverage(p));
    return tinted(crystals * 0.95 + facets + edge * formation * 0.22, crystals);
}

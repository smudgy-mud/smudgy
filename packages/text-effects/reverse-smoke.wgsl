// Three independently advected bodies of the same captured glyph.
const TEXT_FRAGMENTS: u32 = 3u;
struct Parameters { intensity: f32, speed: f32, base: vec4f, bright: vec4f, accent: vec4f }
@group(1) @binding(0) var<uniform> params: Parameters;
fn hash(p: vec2f) -> f32 { return fract(sin(dot(p, vec2f(127.1, 311.7))) * 43758.5453); }
fn noiseGradient(p: vec2f) -> vec3f {
    let i = floor(p); let f = fract(p); let t = f * f * (3.0 - 2.0 * f);
    let a = hash(i); let b = hash(i + vec2f(1, 0));
    let c = hash(i + vec2f(0, 1)); let d = hash(i + vec2f(1, 1));
    let cross = d - c - b + a;
    let dt = 6.0 * f * (1.0 - f);
    return vec3f(mix(mix(a, b, t.x), mix(c, d, t.x), t.y),
        (b - a + cross * t.y) * dt.x, (c - a + cross * t.x) * dt.y);
}
fn rotate(p: vec2f, a: f32) -> vec2f { return vec2f(cos(a) * p.x - sin(a) * p.y, sin(a) * p.x + cos(a) * p.y); }
struct Flow { position: vec2f, dx: vec2f, dy: vec2f }
fn ruffle(field: Flow, clock: f32, seed: f32, amount: f32, wavelength: f32) -> Flow {
    let s = text.effect_scale;
    let a = noiseGradient(field.position / (s * wavelength) * vec2f(0.08, 0.045) + vec2f(seed * 11.0, -clock * 0.22));
    let b = noiseGradient(field.position / (s * wavelength) * vec2f(0.055, 0.075) + vec2f(clock * 0.19, seed * 17.0));
    let gx = a.yz * vec2f(0.08, 0.045) * 7.0 * amount / wavelength;
    let gy = b.yz * vec2f(0.055, 0.075) * 6.0 * amount / wavelength;
    return Flow(field.position + vec2f((a.x - 0.5) * 7.0, (b.x - 0.5) * 6.0) * amount * s,
        field.dx + vec2f(dot(gx, field.dx), dot(gy, field.dx)),
        field.dy + vec2f(dot(gx, field.dy), dot(gy, field.dy)));
}
fn curl(flow: Flow, center: vec2f, radius: f32, turn: f32) -> Flow {
    let d = flow.position - center;
    let angle = turn * exp(-dot(d, d) / (radius * radius));
    let bent = rotate(d, angle);
    let tangent = vec2f(-bent.y, bent.x);
    // Carry the analytic pixel gradient through the folds to antialias their rims.
    let da = -2.0 * angle / (radius * radius);
    return Flow(center + bent,
        rotate(flow.dx, angle) + tangent * (dot(d, flow.dx) * da),
        rotate(flow.dy, angle) + tangent * (dot(d, flow.dy) * da));
}
fn piece(p: vec2f, lo: f32, hi: f32, lod: f32) -> vec4f {
    if p.x < lo || p.x >= hi { return vec4f(0.0); }
    return sampleTextLod(p, lod);
}
struct Ink { color: vec4f, gradient: vec2f }
fn softened(p: vec2f, lo: f32, hi: f32, radius: f32) -> Ink {
    if radius < 0.05 { return Ink(piece(p, lo, hi, 0.0), vec2f(0.0)); }
    let weights = array<f32, 3>(0.25, 0.5, 0.25);
    let step = radius * 0.6;
    // Cached mip filtering connects the footprint even across very tight folds.
    let lod = max(0.0, log2(max(radius * text.scale * 1.2, 1.0)));
    var color = vec4f(0.0);
    var gradient = vec2f(0.0);
    for (var y = 0; y < 3; y++) {
        for (var x = 0; x < 3; x++) {
            let weight = weights[x] * weights[y];
            let offset = vec2f(f32(x) - 1.0, f32(y) - 1.0) * step;
            let ink = piece(p + offset, lo, hi, lod);
            color += ink * weight;
            gradient += ink.a * offset * weight / (step * step);
        }
    }
    return Ink(color, gradient);
}
fn turbulence(p: vec3f, clock: f32, footprint: f32) -> f32 {
    let drift = vec3f(sin(clock * 0.31) * 0.8, cos(clock * 0.27) * 0.6, clock * 0.22);
    var q = p + drift;
    let weights = array<f32, 3>(0.60, 0.28, 0.12);
    var frequency = 1.0;
    var eddies = 0.0;
    for (var octave = 0u; octave < 3u; octave++) {
        let lod = log2(max(footprint * frequency, 1.0));
        eddies += weights[octave] * noise3D(q, lod);
        // Rotate the next scale's field so its eddies do not share an axis.
        q = vec3f(q.y + q.z, q.z - q.x, q.x + q.y) * 1.65 + vec3f(7.0, -3.0, 11.0);
        frequency *= 2.35;
    }
    return eddies;
}
fn smokeDensity(p: vec3f, mask: f32, clock: f32, footprint: f32, seed: f32) -> f32 {
    // The blurred glyph supplies optical thickness, with a parabolic depth
    // profile. Turbulence redistributes that material without a spherical cloud.
    let thickness = mask * mask * 1.6 * max(1.0 - p.z * p.z, 0.0);
    let eddies = turbulence(p + vec3f(seed * 19.0, seed * 41.0, seed * 29.0), clock, footprint);
    return clamp(thickness * (0.85 + eddies * 0.65), 0.0, 1.0);
}
// A bounded, shallow orthographic volume over the glyph-derived density.
// The glyph-derived density field itself contracts; there is no cloud/word crossfade.
fn volume(p: vec2f, ink: Ink, clock: f32, footprint: f32, seed: f32) -> vec2f {
    let mask = pow(max(ink.color.a, 0.0), 0.6);
    let light = normalize(vec3f(-0.5, -1.0, 0.7));
    var transmission = 1.0;
    var illumination = 0.0;
    let gradient = ink.gradient * max(text.text_size.y, 16.0);
    for (var i = 0; i < 3; i++) {
        let at = vec3f(p, 1.0 - (f32(i) + 0.5) / 1.5);
        let density = smokeDensity(at, mask, clock, footprint, seed);
        if density <= 0.001 { continue; }
        // Reuse the analytic glyph gradient for lighting instead of a second
        // density march: three bounded depth samples, no extra shadow lookups.
        let normal = normalize(vec3f(-gradient, 0.45 + abs(at.z)));
        let diffuse = max(dot(normal, light), 0.0);
        let alpha = 1.0 - exp(-density * 1.1);
        illumination += transmission * alpha * (0.35 + diffuse * 0.65);
        transmission *= 1.0 - alpha;
    }
    return vec2f(1.0 - transmission, illumination / max(1.0 - transmission, 0.001));
}

fn condensation() -> f32 {
    if params.intensity <= 0.0 || params.speed <= 0.0 || glyphCount() == 0u { return 1.0; }
    let clock = text.time * params.speed;
    let phase = select(fract(clock / 4.4), text.progress, text.duration > 0.0);
    if text.duration > 0.0 { return smoothstep(0.03, 0.90, phase); }
    return smoothstep(0.02, 0.70, phase) * (1.0 - smoothstep(0.82, 1.0, phase));
}
struct Body {
    lo: f32, hi: f32, root: vec2f, seed: f32, second: f32,
    drift: vec2f, angle: f32, stretch: vec2f,
}
fn bodyAt(instance: u32, gather: f32) -> Body {
    let index = instance / TEXT_FRAGMENTS;
    let glyph = glyphAt(index);
    var lo = glyph.advance.x;
    var hi = glyph.advance.x + glyph.advance.z;
    if index == 0u { lo = -2.0; }
    if index + 1u < glyphCount() { hi = glyphAt(index + 1u).advance.x; }
    // The last admitted glyph retains a long capture's suffix as one smoke body.
    if index + 1u == glyphCount() { hi = text.text_size.x + 2.0; }
    let root = vec2f((lo + hi) * 0.5, text.text_size.y * 0.5);
    let id = f32(instance) + text.seed * 23.0;
    let seed = hash(vec2f(id, 3.0));
    let second = hash(vec2f(id, 7.0));
    let s = text.effect_scale;
    let scatter = 1.0 - gather;
    let opening = 1.0 - smoothstep(0.08, 0.55, gather);
    // Keep the line's vertical anchor fixed; only local folds circulate its ink.
    let drift = vec2f((seed - 0.5) * (25.0 * scatter + 48.0 * opening) * s, 0.0);
    return Body(lo, hi, root, seed, second, drift, (seed - 0.5) * 1.3 * scatter * (1.0 + 4.0 * opening),
        vec2f(1.0 + scatter * s * 0.18, 1.0 + scatter * s * (2.1 + seed * 0.7)));
}

// Every body uses the same ink, following its own path to the identity map.
// Independent quads bound the work to nearby glyphs instead of a pane-wide loop.
fn effect(p: vec2f) -> vec4f {
    if condensation() >= 0.9999 { return sampleText(p); }
    return sampleText(p) * (1.0 - min(params.intensity, 1.0));
}
fn textVertex(instance: u32, corner: vec2f) -> SmudgyFragment {
    let gather = condensation();
    if gather >= 0.9999 { return SmudgyFragment(vec2f(0), vec2f(0), 1.0, 0.0); }
    let body = bodyAt(instance, gather);
    if body.hi <= body.lo { return SmudgyFragment(vec2f(0), vec2f(0), 1.0, 0.0); }
    let scatter = 1.0 - gather;
    let s = text.effect_scale;
    let opening = 1.0 - smoothstep(0.08, 0.55, gather);
    let extent = length(vec2f(body.hi - body.lo, text.text_size.y + 4.0) * body.stretch * 0.5)
        + (62.0 + 48.0 * opening) * s * scatter;
    let position = body.root + body.drift + (corner * 2.0 - 1.0) * extent;
    return SmudgyFragment(position, position, 1.0, min(params.intensity, 1.0));
}
fn textFragment(unused: vec4f, p: vec2f, instance: u32) -> vec4f {
    let gather = condensation();
    let body = bodyAt(instance, gather);
    let s = text.effect_scale;
    if p.x < -144.0 * s || p.x > text.text_size.x + 144.0 * s
        || p.y < -144.0 * s || p.y > text.text_size.y + 144.0 * s { return vec4f(0); }
    let clock = text.time * params.speed;
    let scatter = 1.0 - gather;
    let opening = 1.0 - smoothstep(0.08, 0.55, gather);
    // Fivefold opening distortion melts away before the approved final morph.
    let distortion = 1.0 + 4.0 * opening;
    let light = smoothstep(0.35, 0.80, dot(text.background.rgb, vec3f(0.2126, 0.7152, 0.0722)));
    let pixel = text.surface.x / text.resolution.x;
    let seed = body.seed;
    let second = body.second;
    let q0 = rotate(p - body.root - body.drift, body.angle);
    if abs(q0.x) > (body.hi - body.lo) * body.stretch.x * 0.5 + (46.0 + 32.0 * opening) * s * scatter
        || abs(q0.y) > text.text_size.y * body.stretch.y * 0.5 + (24.0 + 32.0 * opening) * s * scatter { return vec4f(0); }
    var field = Flow(q0, rotate(vec2f(pixel, 0.0), body.angle), rotate(vec2f(0.0, pixel), body.angle));
    let flow = clock * (0.70 + second * 0.33) + seed * 6.2831853
        + sin(clock * 1.7) * opening * 0.9;
    // Distort the eddies themselves so extra strength makes irregular sheets,
    // rather than simply winding the same circular stroke into more tight rings.
    field = ruffle(field, clock * (1.0 + opening * 2.0), seed,
        scatter * opening * 1.7 * distortion, sqrt(distortion));
    field = curl(field, vec2f(9.0 * sin(flow), -18.0 - seed * 10.0) * s,
        (21.0 + seed * 14.0) * s, (1.8 + seed * 2.4 + sin(flow) * 0.7) * scatter * sqrt(distortion));
    field = curl(field, vec2f(-11.0 * cos(flow * 0.8), 20.0 + second * 8.0) * s,
        (18.0 + second * 13.0) * s, (-1.2 - second * 2.2 + cos(flow * 0.7) * 0.6) * scatter * sqrt(distortion));
    field = ruffle(field, clock * (1.0 + opening * 2.0), seed, scatter * distortion, sqrt(distortion));
    let at = body.root + field.position / body.stretch;
    let footprint = max(length(field.dx / body.stretch), length(field.dy / body.stretch));
    let radius = max(scatter * scatter * (2.6 + opening * 2.8), min(footprint * 0.55, 7.0) * scatter);
    let support = radius * 1.5;
    if at.x < body.lo - support || at.x > body.hi + support
        || at.y < -support - 2.0 || at.y > text.text_size.y + support + 2.0 { return vec4f(0); }
    let soft = softened(at, body.lo, body.hi, radius);
    let ink = soft.color;
    let screenGradient = vec2f(dot(soft.gradient, field.dx / body.stretch), dot(soft.gradient, field.dy / body.stretch));
    let threshold = mix(0.13, 0.5, gather);
    let aa = max(length(screenGradient) * 0.85, 0.075 + opening * 0.075);
    let contour = (ink.a - threshold) / aa;
    let rim = exp(-contour * contour) * smoothstep(0.015, 0.07, ink.a);
    let veil = mix(smoothstep(0.02, 0.55, ink.a), 1.0 - exp(-ink.a * 3.0), opening);
    let lighting = 0.65 + 0.35 * noiseGradient(field.position / s * vec2f(0.035, 0.06) + vec2f(seed * 13.0, -clock * 0.17)).x;
    var density = clamp((rim * (0.64 - opening * 0.52) + veil * (0.34 + opening * 0.15)) * lighting, 0.0, 0.92);
    var volumeLight = 1.0;
    if opening > 0.001 && ink.a > 0.002 {
        let unit = max(text.text_size.y, 16.0) * 0.45 * s;
        let cloud = volume(field.position / unit, soft,
            clock * (0.7 + second * 0.4), pixel / unit, seed);
        density = mix(density, cloud.x, opening);
        volumeLight = mix(1.0, cloud.y, opening);
    }
    let material = smoothstep(0.25, 0.98, gather);
    let alpha = mix(density, ink.a, material) * min(params.intensity, 2.0);
    let tint = mix(mix(params.bright, params.accent, rim), params.base, light * 0.85);
    let color = mix(tint.rgb * volumeLight, ink.rgb / max(ink.a, 0.001), material);
    let opacity = clamp(alpha * mix(tint.a, 1.0, material), 0.0, 1.0);
    // Optical density converges to a third of the glyph per body. Their over
    // composition reproduces the captured alpha without thickening the letters.
    let share = mix(0.48, 1.0 / f32(TEXT_FRAGMENTS), smoothstep(0.1, 0.80, gather));
    let split = 1.0 - pow(max(1.0 - opacity, 0.0), share);
    return vec4f(color * split, split);
}

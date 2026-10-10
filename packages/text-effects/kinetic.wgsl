// Seven gestures acting on cached shaped glyphs. Baselines stay fixed at rest.
const TEXT_FRAGMENTS: u32 = 1u;
struct Parameters { intensity: f32, speed: f32, variant: f32, amplitude: f32,
    mode: f32, base: vec4f, bright: vec4f, accent: vec4f }
@group(1) @binding(0) var<uniform> params: Parameters;
fn kh(n: f32) -> f32 { return fract(sin(n * 127.1 + text.seed) * 43758.5453); }
fn kp() -> f32 {
    if params.speed <= 0.0 { return 1.0; }
    return select(fract(text.time * params.speed / 2.4), text.progress, text.duration > 0.0);
}
fn displacement() -> f32 {
    let p = kp();
    if params.mode < 0.5 { return 1.0 - smoothstep(0.0, 0.90, p); }
    if params.mode < 1.5 { return smoothstep(0.04, 0.90, p); }
    return sin(p * 3.141593) * (1.0 - smoothstep(0.80, 0.98, p));
}
fn effect(p: vec2f) -> vec4f {
    if params.intensity * params.amplitude <= 0.0 || params.speed <= 0.0
        || (kp() >= 0.98 && params.mode > 1.5) || glyphsTruncated() { return sampleText(p); }
    return vec4f(0);
}
fn textVertex(instance: u32, corner: vec2f) -> SmudgyFragment {
    let phase = kp();
    if params.intensity * params.amplitude <= 0.0 || params.speed <= 0.0
        || (phase >= 0.98 && params.mode > 1.5) || glyphsTruncated() {
        return SmudgyFragment(vec2f(0), vec2f(0), 1.0, 0.0);
    }
    let glyph = glyphAt(instance);
    var lo = vec2f(glyph.advance.x, -2);
    var hi = vec2f(glyph.advance.x + glyph.advance.z, text.text_size.y + 2.0);
    if instance + 1u < glyphCount() { hi.x = glyphAt(instance + 1u).advance.x; }
    if instance == 0u { lo.x = -2.0; }
    if instance + 1u == glyphCount() { hi.x = text.text_size.x + 2.0; }
    let source = mix(lo, hi, corner);
    let pivot = vec2f((lo.x + hi.x) * 0.5, glyphBaseline(instance));
    let word = vec2f(text.text_size.x * 0.5, textBaseline());
    let x = pivot.x / max(text.text_size.x, 1.0);
    let h = text.text_size.y;
    let power = min(params.intensity * params.amplitude * text.effect_scale, 2.0);
    let d = displacement() * power;
    var offset = vec2f(0);
    var stretch = vec2f(1);
    var angle = 0.0;
    var hinge = pivot;
    var w = 1.0;
    if params.variant < 0.5 {
        // Recoil: horizontal draw-back, release, then damped recoil into the line.
        let pull = sin(clamp(phase / 0.24, 0.0, 1.0) * 1.570796);
        let q = max(phase - 0.24, 0.0);
        let kick = cos(q * (24.0 + params.speed * 6.0)) * exp(-q * 7.0);
        let impulse = select(-pull, -kick, phase >= 0.24) * (1.0 - smoothstep(0.78, 0.98, phase)) * power;
        offset.x = impulse * h * 0.8;
        stretch.x = 1.0 - impulse * 0.13;
        angle = impulse * (x - 0.5) * 0.13;
    } else if params.variant < 1.5 {
        // Accordion: adjacent cards fold in opposite directions, spacing closes as a unit.
        let fold = min(d, 1.0);
        offset.x = (word.x - pivot.x) * fold * 0.88;
        stretch.x = max(0.15, 1.0 - fold * 0.80);
        angle = select(-1.0, 1.0, instance % 2u == 0u) * fold * 0.22;
        offset.y = sin(x * 3.141593) * fold * 2.0;
    } else if params.variant < 2.5 {
        // Pendulum: tops act as hinges, varied weights give a passing impulse.
        hinge.y = max(glyph.ink.y, 0.0);
        let q = max(phase - x * 0.18, 0.0);
        angle = sin(q * (13.0 + params.speed * 3.0) * (0.85 + kh(f32(instance)) * 0.3))
            * exp(-q * 3.4) * smoothstep(0.0, 0.10, q) * (1.0 - smoothstep(0.77, 0.98, phase)) * power * 0.8;
    } else if params.variant < 3.5 {
        // Domino: an edge-on travelling tip, hinged to the baseline, then stand back up.
        let beat = exp(-pow(abs(phase - 0.18 - x * 0.46) / 0.13, 2.0)) * power;
        stretch.x = max(0.08, cos(min(beat, 1.0) * 1.48));
        angle = beat * 0.18;
        offset.x = beat * h * 0.12;
    } else if params.variant < 4.5 {
        // Tumble: whole letters roll in from the left, with a last soft bounce.
        let delay = x * 0.14;
        let residual = 1.0 - smoothstep(delay, 0.72 + delay, phase);
        let r = select(d, residual * power, params.mode < 0.5);
        offset.x = -r * max(text.surface.x * 0.5, text.text_size.x);
        offset.y = -abs(sin(r * 9.0 + x * 0.5)) * r * h * 0.55;
        angle = -r * 8.0;
    } else if params.variant < 5.5 {
        // Magnetic: hovering cards progressively lock; neighbouring letters arrive in a wave.
        let residual = 1.0 - smoothstep(x * 0.16, 0.64 + x * 0.16, phase);
        let r = select(d, residual * power, params.mode < 0.5);
        let seed = f32(instance) + 7.0;
        offset = vec2f(kh(seed) - 0.5, kh(seed + 1.0) - 0.5) * h * 4.0 * r;
        offset += vec2f(sin(phase * 35.0 + x * 12.0), cos(phase * 29.0 + x * 7.0)) * r * 2.0;
        angle = (kh(seed + 2.0) - 0.5) * r * 2.0;
        stretch = vec2f(1.0 + sin(phase * 23.0) * r * 0.08);
    } else {
        // Vortex: intact glyphs spiral around the word, then unfurl in reading order.
        let a = d * (5.5 + x * 2.5);
        let radius = d * min(text.surface.y * 0.38, h * 4.0);
        let v = pivot - word;
        offset = vec2f(v.x * cos(a) - v.y * sin(a), v.x * sin(a) + v.y * cos(a)) - v
            + vec2f(cos(a + x * 6.283185), sin(a + x * 6.283185)) * radius;
        angle = a;
        w = max(0.55, 1.0 + sin(a) * d * 0.25);
    }
    let local = (source - hinge) * stretch;
    let rotated = vec2f(local.x * cos(angle) - local.y * sin(angle), local.x * sin(angle) + local.y * cos(angle));
    let position = hinge + offset + rotated / w;
    let opacity = select(1.0, smoothstep(0.0, 0.06, phase), params.mode < 0.5);
    return SmudgyFragment(position, source, w, opacity);
}
fn textFragment(color: vec4f, source: vec2f, instance: u32) -> vec4f {
    let glint = pow(max(0.0, sin(kp() * 6.283185 - source.x / max(text.text_size.x, 1.0) * 4.0)), 8.0)
        * displacement() * 0.12;
    return vec4f(mix(color.rgb, params.accent.rgb * color.a, glint), color.a);
}

// Paint-only typography, anchored to the shaped font baseline throughout.
struct Parameters {
    intensity: f32, speed: f32, peak: f32, yaw: f32,
    weight: f32, pivot: f32, fit: f32,
    base: vec4f, bright: vec4f, accent: vec4f,
}
@group(1) @binding(0) var<uniform> params: Parameters;
fn calm(a: f32, b: f32, x: f32) -> f32 {
    let q = clamp((x - a) / (b - a), 0.0, 1.0);
    return q * q * q * (q * (q * 6.0 - 15.0) + 10.0);
}

fn effect(p: vec2f) -> vec4f {
    let phase = select(fract(text.time * params.speed / 2.4), text.progress, text.duration > 0.0);
    if phase <= 0.0 || phase >= 1.0 || params.intensity <= 0.0 || params.speed <= 0.0 { return sampleText(p); }
    // Continuous velocity and acceleration at every join. No staged snap or recoil.
    let gather = calm(0.02, 0.46, phase);
    let surge = gather;
    let release = calm(0.58, 0.99, phase);
    let energy = (1.0 - release) * min(params.intensity, 2.0) * text.effect_scale;
    let peak = max(params.peak, 1.0);
    let growth = (peak - 1.0) * gather * energy;
    var zoom = 1.0 + growth;
    let angle = params.yaw * sin(phase * 3.141593) * gather * (1.0 - release);
    let foreshorten = max(cos(angle), 0.65);
    let perspective = 0.2 * abs(sin(angle));
    let padded = text.text_size + vec2f(4.0);
    let pivot = vec2f(text.text_size.x * params.pivot, textBaseline());
    // Reduce growth near a vertical edge rather than lifting the baseline.
    if params.fit > 0.5 {
        let available = max(text.surface - vec2f(8.0), vec2f(1.0));
        let above = max(pivot.y - text.paint_offset.y - 2.0, 0.0);
        let below = max(text.paint_offset.y + text.surface.y - pivot.y - 2.0, 0.0);
        let vertical = min(above / max(pivot.y + 2.0, 1.0), below / max(text.text_size.y - pivot.y + 2.0, 1.0));
        let limit = min(available.x / padded.x, vertical * (1.0 - perspective));
        let capacity = max(limit - 1.0, 0.0);
        // A smooth saturation avoids a sudden velocity change when growth meets an edge.
        zoom = 1.0 + capacity * (1.0 - exp(-growth / max(capacity, 0.001)));
    }
    var destination = pivot;
    if params.fit > 0.5 {
        let left = (pivot.x + 2.0) * zoom * foreshorten;
        let right = (text.text_size.x - pivot.x + 2.0) * zoom * foreshorten;
        let low = text.paint_offset.x + left + 2.0;
        let high = max(low, text.paint_offset.x + text.surface.x - right - 2.0);
        destination.x = mix(pivot.x, clamp(pivot.x, low, high), calm(0.0, 0.5, growth));
    }
    let q = p - destination;
    let depth = 1.0 + q.x / max(text.text_size.x * zoom, 1.0) * sin(angle) * 0.4;
    let source = vec2f(q.x / foreshorten, q.y * depth) / zoom + pivot;
    // Pane overflow does not require nine texture reads on pixels outside the moving word.
    if any(source < vec2f(-2.0)) || any(source > text.text_size + vec2f(2.0)) { return vec4f(0.0); }
    var ink = sampleText(source);
    // A small dilation suggests heavier ink. It preserves each run's captured colour.
    let stroke = min(params.weight * surge * energy * 0.55, 1.5);
    for (var i = 0u; i < 8u; i++) {
        let theta = f32(i) * 0.7853982;
        let neighbour = sampleText(source + vec2f(cos(theta), sin(theta)) * stroke);
        if neighbour.a > ink.a { ink = neighbour; }
    }
    // One restrained travelling highlight during the surge; no surrounding particle field.
    let sweep = exp(-pow(abs(source.x / max(text.text_size.x, 1.0) - (phase - 0.14) * 3.5) * 12.0, 2.0));
    let shine = sweep * surge * (1.0 - smoothstep(0.38, 0.55, phase)) * min(energy, 1.0) * 0.18;
    let tint = mix(mix(params.base, params.bright, surge), params.accent, sweep);
    ink = vec4f(mix(ink.rgb, tint.rgb * ink.a, shine * tint.a), ink.a);
    return ink;
}

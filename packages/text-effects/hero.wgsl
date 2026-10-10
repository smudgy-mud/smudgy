// Six short cinematic poses. Text is the spectacle; light follows its impact.
const TEXT_FRAGMENTS: u32 = 4u;
struct Parameters { intensity: f32, speed: f32, variant: f32, amplitude: f32,
    base: vec4f, bright: vec4f, accent: vec4f }
@group(1) @binding(0) var<uniform> params: Parameters;
fn hh(n: f32) -> f32 { return fract(sin(n * 127.1 + text.seed) * 43758.5453); }
fn hp() -> f32 {
    if params.speed <= 0.0 { return 1.0; }
    return select(fract(text.time * params.speed / 2.4), text.progress, text.duration > 0.0);
}
fn heroPower() -> f32 { return min(params.intensity * params.amplitude * text.effect_scale, 2.0); }
fn heroBeat() -> f32 {
    if params.variant < 0.5 { return 0.42; }
    if params.variant < 1.5 { return 0.48; }
    if params.variant < 2.5 { return 0.40; }
    if params.variant < 3.5 { return 0.35; }
    if params.variant < 4.5 { return 0.52; }
    return 0.46;
}
fn effect(p: vec2f) -> vec4f {
    let phase = hp(); let power = heroPower();
    if phase >= 0.98 || power <= 0.0 || params.speed <= 0.0 { return sampleText(p); }
    let center = vec2f(text.text_size.x * 0.5, textBaseline() - text.text_size.y * 0.4);
    let q = p - center;
    let age = phase - heroBeat();
    let release = 1.0 - smoothstep(0.66, 0.96, phase);
    var light = 0.0;
    if params.variant > 1.5 && params.variant < 2.5 {
        // The slit opens vertically, then contracts behind the turning glyphs.
        let opening = sin(phase * 3.141593) * power;
        let slit = exp(-abs(q.x) / max(1.0, 2.0 * opening))
            * (1.0 - smoothstep(text.text_size.y * (0.4 + opening * 2.0), text.text_size.y * (0.8 + opening * 3.0), abs(q.y)));
        light = slit * opening * 0.75;
    } else if params.variant > 0.5 && params.variant < 1.5 {
        let loom = smoothstep(0.0, 0.35, phase) * release;
        let radius = text.text_size.y * (1.0 + loom * 2.8) * text.effect_scale;
        let ellipse = length(q * vec2f(0.65, 1.0));
        let angle = atan2(q.y, q.x * 0.65);
        let eclipse = exp(-pow(abs(ellipse - radius) / 1.8, 2.0));
        let halo = exp(-pow(abs(ellipse - radius) / 8.0, 2.0)) * 0.18;
        let gleam = pow(max(cos(angle - text.time * params.speed * 0.8), 0.0), 12.0);
        light = (eclipse * (0.18 + gleam * 1.8) + halo) * loom;
    } else {
        let burst = smoothstep(-0.04, 0.01, age) * exp(-max(age, 0.0) * 4.5) * release;
        let expansion = smoothstep(-0.025, 0.22, age);
        let radius = max(min(text.surface.x, text.surface.y), 90.0) * (0.10 + expansion * 0.9) * text.effect_scale;
        // Broad bars have different starts, widths, lengths and slow opposed rotations.
        for (var i = 0u; i < 12u; i++) {
            let n = f32(i);
            let direction = select(-1.0, 1.0, i % 2u == 0u);
            let angle = n * 2.399963 + text.time * params.speed * direction * (0.035 + hh(n + 7.0) * 0.075);
            let along = dot(q, vec2f(cos(angle),sin(angle)));
            let across = abs(dot(q, vec2f(-sin(angle),cos(angle))));
            let width = (1.5 + hh(n + 2.0) * 8.5) * power;
            let start = text.text_size.y * (0.8 + hh(n + 3.0) * 1.4);
            let end = start + radius * (0.4 + hh(n + 4.0) * 0.8);
            let bar = (1.0 - smoothstep(width, width + 1.5, across)) * smoothstep(start,start + 8.0,along)
                * (1.0 - smoothstep(end, end + 22.0, along));
            light += bar * burst * (0.65 + hh(n + 5.0) * 1.1);
        }
        let waveRadius = max(age,0.0) * min(text.surface.x,text.surface.y) * 2.0 * text.effect_scale;
        light += exp(-pow(abs(length(q) - waveRadius) / 2.0, 2.0)) * burst * 0.3;
        if params.variant > 3.5 && params.variant < 4.5 && age >= 0.0 && age < 0.20 {
            // pow needs a nonnegative base, even for an even exponent in WGSL.
            light += exp(-pow(abs(q.x) / max(text.text_size.y * 0.65, 1.0), 4.0))
                * (1.0 - smoothstep(8.0,20.0,q.y)) * exp(-age * 28.0) * 0.75;
        }
    }
    let alpha = min(light * power,0.94) * params.accent.a;
    let tint = mix(params.bright.rgb,params.accent.rgb,min(light,1.0));
    return vec4f(tint * alpha,alpha);
}
fn textVertex(instance: u32, corner: vec2f) -> SmudgyFragment {
    let phase = hp(); let power = heroPower();
    let glyphIndex = instance / TEXT_FRAGMENTS; let piece = instance % TEXT_FRAGMENTS;
    let judgement = params.variant > 3.5 && params.variant < 4.5;
    let ascension = params.variant >= 4.5;
    if phase >= 0.98 || power <= 0.0 || params.speed <= 0.0 || (!judgement && !ascension && piece > 0u)
        || (glyphsTruncated() && glyphIndex > 0u) {
        return SmudgyFragment(vec2f(0),vec2f(0),1.0,0.0);
    }
    let glyph = glyphAt(glyphIndex);
    var lo = vec2f(glyph.advance.x,-2);
    var hi = vec2f(glyph.advance.x + glyph.advance.z,text.text_size.y + 2.0);
    if glyphIndex + 1u < glyphCount() { hi.x = glyphAt(glyphIndex + 1u).advance.x; }
    if glyphIndex == 0u { lo.x = -2.0; }
    if glyphIndex + 1u == glyphCount() { hi.x = text.text_size.x + 2.0; }
    if glyphsTruncated() { lo = vec2f(-2); hi = text.text_size + vec2f(2); }
    if judgement {
        let width = (hi.x-lo.x) / 4.0;
        lo.x += f32(piece) * width; hi.x = lo.x + width;
    }
    let source = mix(lo,hi,corner);
    let word = vec2f(text.text_size.x * 0.5,textBaseline());
    let center = vec2f((lo.x+hi.x)*0.5,textBaseline());
    let release = 1.0 - smoothstep(0.62,0.98,phase);
    var zoom = 1.0; var offset = vec2f(0); var scale = vec2f(1);
    var angle = 0.0; var yaw = 0.0; var opacity = 1.0;
    if params.variant < 0.5 {
        let residual = 1.0 - smoothstep(0.0,0.42,phase);
        offset.x = select(-1.0,1.0,center.x >= word.x) * residual * text.surface.x * 0.55 * power;
        zoom += smoothstep(0.20,0.41,phase) * release * power * 0.65;
        scale.x -= exp(-max(phase-0.42,0.0)*24.0) * step(0.42,phase) * power * 0.25;
        opacity = smoothstep(0.0,0.07,phase);
    } else if params.variant < 1.5 {
        zoom += smoothstep(0.0,0.35,phase) * release * power * 1.65;
        yaw = sin(phase * 3.141593) * 0.16 * power;
    } else if params.variant < 2.5 {
        let residual = 1.0 - smoothstep(0.04,0.66,phase);
        yaw = residual * 1.50;
        offset.x = (word.x-center.x) * residual;
        zoom += sin(phase*3.141593) * release * power * 0.45;
        opacity = smoothstep(0.0,0.07,phase);
    } else if params.variant < 3.5 {
        let residual = 1.0 - smoothstep(0.0,0.35,phase);
        zoom += residual * power * 2.4;
        angle = residual * -0.16 * power;
        offset.x = -residual * text.surface.x * 0.28 * power;
        scale.y -= exp(-max(phase-0.35,0.0)*18.0) * step(0.35,phase) * power * 0.25;
        opacity = smoothstep(0.0,0.05,phase);
    } else if judgement {
        let residual = pow(1.0 - clamp(phase/0.52,0.0,1.0),0.75 + hh(f32(instance)) * 1.4);
        offset.y = -residual * text.surface.y * 0.9 * power;
        scale.y += residual * power * 2.5;
        zoom += smoothstep(0.35,0.52,phase) * release * power * 0.5;
        opacity = smoothstep(0.0,0.07,phase);
    } else {
        let lift = sin(phase*3.141593) * release * power;
        zoom += lift * 1.15;
        offset.y = -lift * text.text_size.y * 0.75;
        if piece > 0u {
            let echo = f32(piece);
            offset.y += echo * text.text_size.y * lift * 0.85;
            zoom += echo * lift * 0.13;
            opacity = lift * 0.13 * (1.0 - smoothstep(0.48,0.85,phase));
        }
    }
    // Large poses fit horizontally without translating their font baseline.
    zoom = min(zoom,max(1.0,(text.surface.x-12.0)/max(text.text_size.x+4.0,1.0)));
    let local = (source-center) * scale;
    let tilted = vec2f(local.x*cos(angle)-local.y*sin(angle),local.x*sin(angle)+local.y*cos(angle));
    let camera = max(text.text_size.y*8.0,180.0);
    let w = max(0.5,1.0 - tilted.x*sin(yaw)/camera);
    let position = word + (center-word+vec2f(tilted.x*cos(yaw),tilted.y))*zoom/w + offset;
    return SmudgyFragment(position,source,w,opacity);
}
fn textFragment(color: vec4f, source: vec2f, instance: u32) -> vec4f {
    let phase = hp();
    var ink = color;
    if params.variant > 0.5 && params.variant < 1.5 {
        let silhouette = smoothstep(0.08,0.34,phase) * (1.0-smoothstep(0.55,0.90,phase));
        let edge = max(max(coverage(source+vec2f(1,0)),coverage(source-vec2f(1,0))),
            max(coverage(source+vec2f(0,1)),coverage(source-vec2f(0,1)))) - color.a;
        ink = vec4f(color.rgb*(1.0-silhouette*0.86)+params.accent.rgb*max(edge,0.0)*silhouette,
            max(color.a,max(edge,0.0)*silhouette));
    }
    // Background-coloured stroke makes white text legible across the wide light bars.
    var outline = 0.0;
    for (var i=0u;i<4u;i++) {
        let a=f32(i)*1.570796;
        outline=max(outline,coverage(source+vec2f(cos(a),sin(a))*1.0));
    }
    let shadow=outline*(1.0-ink.a);
    return vec4f(ink.rgb+text.background.rgb*shadow,ink.a+shadow);
}

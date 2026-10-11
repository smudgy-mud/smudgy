// Quiet, contradictory impressions and displaced strips of the actual glyph ink.
const TEXT_FRAGMENTS: u32 = 4u;
struct Parameters { intensity: f32, speed: f32, variant: f32, amplitude: f32,
    base: vec4f, bright: vec4f, accent: vec4f }
@group(1) @binding(0) var<uniform> params: Parameters;
fn phase() -> f32 { return select(fract(text.time * params.speed / 3.7), text.progress, text.duration > 0.0); }
fn envelope(p: f32) -> f32 { return smoothstep(0.03,0.23,p) * (1.0-smoothstep(0.70,0.98,p)); }
fn power() -> f32 { return min(params.intensity * params.amplitude * text.effect_scale, 2.0); }
fn quiet() -> bool { return params.speed <= 0.0 || power() <= 0.0 || envelope(phase()) < 0.0001 || glyphsTruncated(); }
fn wave(p: f32, x: f32) -> f32 { return sin(p * 6.2831853 + x * 2.3) * envelope(p); }
fn effect(p: vec2f) -> vec4f { if quiet() { return sampleText(p); } return vec4f(0); }
fn textVertex(instance: u32, corner: vec2f) -> SmudgyFragment {
    if quiet() { return SmudgyFragment(vec2f(0),vec2f(0),1.0,0.0); }
    let index = instance / TEXT_FRAGMENTS;
    let piece = instance % TEXT_FRAGMENTS;
    let glyph = glyphAt(index);
    var lo = vec2f(glyph.advance.x,-2.0);
    var hi = vec2f(glyph.advance.x+glyph.advance.z,text.text_size.y+2.0);
    if index == 0u { lo.x = -2.0; }
    if index+1u < glyphCount() { hi.x = glyphAt(index+1u).advance.x; }
    if index+1u == glyphCount() { hi.x = text.text_size.x+2.0; }
    let p = phase(); let x = f32(index) * 0.37;
    let pivot = vec2f((lo.x+hi.x)*0.5,glyphBaseline(index));
    let h = text.text_size.y;
    let strength = power();
    var offset = vec2f(0.0); var angle = 0.0; var shear = 0.0; var opacity = 1.0;
    if params.variant < 0.5 {
        var pose = wave(p,x);
        if piece < 3u {
            // One impression lags; the other two move before the real letter.
            let timing = select(-0.09, 0.13 + f32(piece)*0.035, piece>0u);
            pose = wave(clamp(p+timing,0.0,1.0),x);
            offset.x = (pose-wave(p,x)) * h * 0.46 * strength
                + select(-1.0,1.0,piece>0u) * envelope(p)*h*0.08*strength;
            opacity = envelope(p) * select(0.20,0.13,piece>0u);
        }
        shear = pose * 0.13 * strength;
        angle = pose * 0.035 * strength;
    } else {
        let cell = (hi.y-lo.y)/4.0;
        lo.y += f32(piece)*cell; hi.y = lo.y+cell;
        let interval = sin(p*3.141593);
        let creep = sin(p*5.6+x+f32(piece)*1.9) * envelope(p);
        offset.x = creep * h * 0.33 * strength;
        angle = creep * (f32(piece)-1.5) * 0.19 * strength;
        shear = sin(x+f32(piece)*2.2) * interval * envelope(p) * 0.42 * strength;
    }
    let source = mix(lo,hi,corner);
    var local = source-pivot;
    local.x += local.y*shear;
    let rotated = vec2f(local.x*cos(angle)-local.y*sin(angle),local.x*sin(angle)+local.y*cos(angle));
    return SmudgyFragment(pivot+rotated+offset,source,1.0,opacity);
}
fn textFragment(color: vec4f, source: vec2f, instance: u32) -> vec4f {
    if params.variant<0.5 && instance%TEXT_FRAGMENTS<3u {
        let light = smoothstep(0.35,0.8,dot(text.background.rgb,vec3f(0.2126,0.7152,0.0722)));
        let tint = mix(params.bright,params.base,light);
        return vec4f(tint.rgb*color.a*tint.a,color.a*tint.a);
    }
    let edge = smoothstep(0.15,0.85,color.a)*envelope(phase())*0.11;
    return vec4f(mix(color.rgb,params.accent.rgb*color.a,edge),color.a);
}

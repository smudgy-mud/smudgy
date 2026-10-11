// Captured strokes turn into veined leaves, tumble, and reassemble on their baseline.
const TEXT_FRAGMENTS: u32 = 9u;
struct Parameters { intensity: f32, speed: f32, amplitude: f32, mode: f32,
    base: vec4f, bright: vec4f, accent: vec4f }
@group(1) @binding(0) var<uniform> params: Parameters;
fn ah(n: f32) -> f32 { return fract(sin(n*127.1+text.seed)*43758.5453); }
fn dispersion() -> f32 {
    if params.intensity<=0.0 || params.speed<=0.0 || params.amplitude<=0.0 { return 0.0; }
    let p=select(fract(text.time*params.speed/3.4),text.progress,text.duration>0.0);
    if params.mode<0.5 { return 1.0-smoothstep(0.03,0.94,p); }
    if params.mode<1.5 { return smoothstep(0.03,0.94,p); }
    return smoothstep(0.03,0.44,p)*(1.0-smoothstep(0.56,0.97,p));
}
fn effect(p: vec2f) -> vec4f {
    if dispersion()<0.0001 { return sampleText(p); }
    return vec4f(0);
}
fn cellBounds(index: u32) -> vec4f {
    let glyph=glyphAt(index/9u);
    var lo=vec2f(glyph.advance.x,-2.0);
    var hi=vec2f(glyph.advance.x+glyph.advance.z,text.text_size.y+2.0);
    if index/9u+1u<glyphCount() { hi.x=glyphAt(index/9u+1u).advance.x; }
    if index/9u==0u { lo.x=-2.0; }
    if index/9u+1u==glyphCount() { hi.x=text.text_size.x+2.0; }
    if glyphsTruncated() { lo=vec2f(-2);hi=text.text_size+vec2f(2); }
    let size=(hi-lo)/3.0;
    let tile=vec2f(f32(index%3u),f32((index%9u)/3u));
    return vec4f(lo+tile*size,size);
}
fn textVertex(index: u32, corner: vec2f) -> SmudgyFragment {
    let d=dispersion();
    if d<0.0001 || (glyphsTruncated() && index/9u>0u) {
        return SmudgyFragment(vec2f(0),vec2f(0),1.0,0.0);
    }
    let bounds=cellBounds(index);let center=bounds.xy+bounds.zw*0.5;
    let seed=f32(index)+13.0;let power=min(params.intensity*params.amplitude*text.effect_scale,3.0);
    let delayed=clamp(d*(0.85+ah(seed)*0.30),0.0,1.0);
    let travel=pow(delayed,1.4)*power;
    let flutter=sin(delayed*(7.0+ah(seed+1.0)*5.0)+ah(seed+2.0)*6.283185);
    let offset=vec2f((ah(seed+3.0)-0.5)*105.0+flutter*18.0,
        48.0+ah(seed+4.0)*48.0)*travel;
    let angle=delayed*(ah(seed+5.0)-0.5)*10.0;
    let grow=mix(1.0,1.4+ah(seed+6.0),delayed);
    let source=bounds.xy+corner*bounds.zw;
    // Avoid a discontinuous width change at the moment ink separates.
    let stable=(source-center)*mix(vec2f(1),vec2f(max(0.30,abs(cos(delayed*5.0+ah(seed)*2.0))),1)*grow,smoothstep(0.0,0.2,delayed));
    let rotated=vec2f(stable.x*cos(angle)-stable.y*sin(angle),stable.x*sin(angle)+stable.y*cos(angle));
    var ink=0.0;
    for(var y=0u;y<3u;y++){for(var x=0u;x<3u;x++){
        ink=max(ink,coverage(bounds.xy+bounds.zw*(vec2f(f32(x),f32(y))+vec2f(0.5))/3.0));
    }}
    return SmudgyFragment(center+offset+rotated,source,1.0,mix(1.0,ink,smoothstep(0.05,0.55,d)));
}
fn textFragment(color: vec4f, source: vec2f, index: u32) -> vec4f {
    let bounds=cellBounds(index);let uv=(source-bounds.xy)/bounds.zw*2.0-1.0;
    let d=smoothstep(0.05,0.55,dispersion());
    let silhouette=1.0-smoothstep(0.0,0.14,abs(uv.x)-0.78*(1.0-uv.y*uv.y));
    let leaf= silhouette*(1.0-smoothstep(0.86,1.0,abs(uv.y)));
    let vein=exp(-abs(uv.x)*25.0)+exp(-abs(fract(uv.y*3.0+abs(uv.x)*1.7)-0.5)*25.0)*0.45;
    let palette=mix(params.base,params.bright,ah(f32(index)+22.0));
    let tint=mix(palette,params.accent,clamp(vein*0.4,0.0,0.7));
    return mix(color,vec4f(tint.rgb*leaf*tint.a,leaf*tint.a),d);
}

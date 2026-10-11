// A memory loses parts of its letters, then retrieves them in several uncertain attempts.
const TEXT_FRAGMENTS:u32=12u;
struct Parameters { intensity:f32,speed:f32,base:vec4f,bright:vec4f,accent:vec4f }
@group(1) @binding(0) var<uniform> params:Parameters;
fn hash(n:f32)->f32{return fract(sin(n*127.1+text.seed)*43758.5453);}
fn phase()->f32{
    if text.duration<=0.0{return fract(text.time*params.speed/4.4);}
    let rate=clamp(params.speed,0.25,4.0);
    return text.progress*rate/(1.0+text.progress*(rate-1.0));
}
fn power()->f32{
    if params.intensity<=0.0 || params.speed<=0.0{return 0.0;}
    return min(params.intensity,1.0)*smoothstep(0.0,0.10,phase())*(1.0-smoothstep(0.94,0.995,phase()));
}
fn column(index:u32)->vec4f{
    let glyph=glyphAt(index);
    let first=index==0u || glyphAt(index-1u).cluster.z!=glyph.cluster.z;
    let last=index+1u==glyphCount() || glyphAt(index+1u).cluster.z!=glyph.cluster.z;
    var lo=glyph.advance.xy-vec2f(0,2);
    var hi=glyph.advance.xy+glyph.advance.zw+vec2f(0,2);
    if !last{hi.x=glyphAt(index+1u).advance.x;}
    if first{lo.x=min(lo.x,glyph.ink.x)-2.0;}
    if last{hi.x=max(hi.x,glyph.ink.x+glyph.ink.z)+2.0;}
    if glyphsTruncated(){lo=vec2f(-2);hi=text.text_size+vec2f(2);}
    return vec4f(lo,hi-lo);
}
fn bounds(instance:u32)->vec4f{
    let box=column(instance/12u);let size=box.zw/vec2f(3,4);
    let cell=vec2f(f32(instance%3u),f32((instance%12u)/3u));
    return vec4f(box.xy+cell*size,size);
}
fn lost(instance:u32)->f32{
    let t=phase();let seed=hash(f32(instance)+4.0);
    let x=column(instance/12u).x/max(text.text_size.x,1.0);
    let onset=0.06+(1.0-x)*0.22+seed*0.09;
    let forgotten=smoothstep(onset,onset+0.18,t);
    let restore=0.55+seed*0.22+x*0.08;
    let recalled=smoothstep(restore,restore+0.14,t);
    // Two incomplete recollections precede the real one, with eased edges rather than flicker.
    let attempt1=smoothstep(0.43+seed*0.08,0.49+seed*0.08,t)*(1.0-smoothstep(0.51+seed*0.08,0.56+seed*0.08,t));
    let attempt2=smoothstep(0.59+seed*0.04,0.64+seed*0.04,t)*(1.0-smoothstep(0.67+seed*0.04,0.72+seed*0.04,t));
    return forgotten*(1.0-recalled)*(1.0-attempt1*0.55)*(1.0-attempt2*0.35)*power();
}
fn effect(p:vec2f)->vec4f{
    if power()<0.0001{return sampleText(p);}
    return vec4f(0);
}
fn textVertex(instance:u32,corner:vec2f)->SmudgyFragment{
    if power()<0.0001 || (glyphsTruncated() && instance/12u>0u){return SmudgyFragment(vec2f(0),vec2f(0),1,0);}
    let box=bounds(instance);let source=box.xy+corner*box.zw;let loss=lost(instance);
    let doubt=loss*(1.0-loss)*4.0*smoothstep(0.43,0.60,phase());
    let seed=hash(f32(instance)+11.0);
    let offset=vec2f(seed-0.5,hash(f32(instance)+15.0)-0.5)*doubt*1.8*text.effect_scale;
    return SmudgyFragment(source+offset,source,1,1);
}
fn textFragment(original:vec4f,p:vec2f,instance:u32)->vec4f{
    let loss=lost(instance);
    // Broad absences have a faint last impression, rather than turning into a particle field.
    let ink=sampleTextLod(p,loss*0.85*text.effect_scale);
    let a=ink.a*(1.0-loss*0.94);
    let light=smoothstep(0.3,0.8,dot(text.background.rgb,vec3f(0.2126,0.7152,0.0722)));
    let faded=mix(params.bright.rgb,params.base.rgb,light);
    let color=mix(ink.rgb/max(ink.a,0.001),faded,loss*0.45);
    return vec4f(color*a,a);
}

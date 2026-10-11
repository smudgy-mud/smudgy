// Two deadlines advance through readable ink. A local crossed fault marks the decisive instant.
const TEXT_FRAGMENTS:u32=1u;
struct Parameters { intensity:f32,speed:f32,base:vec4f,bright:vec4f,accent:vec4f }
@group(1) @binding(0) var<uniform> params:Parameters;
fn phase()->f32 {
    if text.duration<=0.0 { return fract(text.time*params.speed/3.2); }
    let rate=clamp(params.speed,0.25,4.0);
    return text.progress*rate/(1.0+text.progress*(rate-1.0));
}
fn power()->f32 {
    if params.intensity<=0.0 || params.speed<=0.0 { return 0.0; }
    return min(params.intensity,1.0)*smoothstep(0.0,0.12,phase())*(1.0-smoothstep(0.80,0.995,phase()));
}
fn pressure()->f32 { return smoothstep(0.07,0.67,phase())*(1.0-smoothstep(0.69,0.85,phase()))*power(); }
fn center()->vec2f {
    var index=glyphCount()/2u;
    if glyphsTruncated() { return vec2f(text.text_size.x*0.5,textBaseline()-text.text_size.y*0.3); }
    if any(glyphAt(index).ink.zw<=vec2f(0)) && index+1u<glyphCount() { index+=1u; }
    let g=glyphAt(index);
    return g.ink.xy+g.ink.zw*0.5;
}
fn column(index:u32)->vec4f {
    let g=glyphAt(index);var lo=g.advance.xy-vec2f(0,2);var hi=g.advance.xy+g.advance.zw+vec2f(0,2);
    let first=index==0u || glyphAt(index-1u).cluster.z!=g.cluster.z;
    let last=index+1u==glyphCount() || glyphAt(index+1u).cluster.z!=g.cluster.z;
    if !last { hi.x=glyphAt(index+1u).advance.x; }
    if first { lo.x=min(lo.x,g.ink.x)-2.0; }
    if last { hi.x=max(hi.x,g.ink.x+g.ink.z)+2.0; }
    if glyphsTruncated() { lo=vec2f(-2);hi=text.text_size+vec2f(2); }
    return vec4f(lo,hi-lo);
}
fn effect(p:vec2f)->vec4f { if power()<0.0001 { return sampleText(p); } return vec4f(0); }
fn textVertex(index:u32,corner:vec2f)->SmudgyFragment {
    if power()<0.0001 || (glyphsTruncated() && index>0u) { return SmudgyFragment(vec2f(0),vec2f(0),1,0); }
    let box=column(index);let source=box.xy+corner*box.zw;let pivot=center();
    let compress=1.0-pressure()*0.10*min(text.effect_scale,2.5)*sqrt(params.intensity);
    return SmudgyFragment(vec2f(pivot.x+(source.x-pivot.x)*compress,source.y),source,1,1);
}
fn textFragment(ink:vec4f,p:vec2f,index:u32)->vec4f {
    let t=phase();let s=text.effect_scale;let pivot=center();
    let leftSpan=max(pivot.x,1.0);let rightSpan=max(text.text_size.x-pivot.x,1.0);
    let distance=select((pivot.x-p.x)/leftSpan,(p.x-pivot.x)/rightSpan,p.x>pivot.x);
    let remaining=1.0-pow(smoothstep(0.08,0.67,t),1.8);
    let approaching=exp(-pow((distance-remaining)/0.10,2.0))*power();
    let behind=smoothstep(remaining,remaining+0.08,distance)*pressure();
    let heat=mix(params.bright.rgb,params.base.rgb,smoothstep(0.33,0.66,t));
    let native=ink.rgb/max(ink.a,0.001);
    var colour=mix(native,heat,behind*0.78);
    colour=mix(colour,params.accent.rgb,approaching*0.80);
    let strike=smoothstep(0.66,0.69,t)*(1.0-smoothstep(0.72,0.81,t))*power();
    let q=p-pivot;
    let slash=min(abs(q.x-q.y*0.65),abs(q.x+q.y*0.65));
    let local=1.0-smoothstep(6.0*s,11.0*s,length(q));
    let cut=(1.0-smoothstep(0.2*s,1.0*s,slash))*strike*local;
    let rim=exp(-pow((slash-1.35*s)/(0.55*s),2.0))*strike*local;
    let span=select(leftSpan,rightSpan,p.x>pivot.x);
    let fault=abs((distance-remaining)*span+(p.y-pivot.y)*0.24);
    let chasing=(1.0-smoothstep(0.55*s,1.55*s,fault))*power()*(1.0-smoothstep(0.65,0.69,t));
    let alpha=ink.a*(1.0-max(cut*0.90,chasing*0.85));
    return vec4f(mix(colour,params.bright.rgb,rim*0.75)*alpha,alpha);
}

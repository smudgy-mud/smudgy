// Fine bindings constrain individual glyphs. The inscription resists, locks, and is released.
const TEXT_FRAGMENTS:u32=2u;
struct Parameters { intensity:f32,speed:f32,base:vec4f,bright:vec4f,accent:vec4f }
@group(1) @binding(0) var<uniform> params:Parameters;
fn phase()->f32 {
    if text.duration<=0.0 { return fract(text.time*params.speed/4.0); }
    let rate=clamp(params.speed,0.25,4.0);
    return text.progress*rate/(1.0+text.progress*(rate-1.0));
}
fn power()->f32 {
    if params.intensity<=0.0 || params.speed<=0.0 { return 0.0; }
    return min(params.intensity,1.0)*smoothstep(0.0,0.12,phase())*(1.0-smoothstep(0.77,0.98,phase()));
}
fn bound(index:u32)->f32 {
    let x=glyphAt(index).advance.x/max(text.text_size.x,1.0);
    return smoothstep(0.08+x*0.28,0.19+x*0.28,phase())*power();
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
fn tension(index:u32)->f32 {
    let seed=fract(sin(f32(index)*127.1+text.seed)*43758.5453);
    let struggle=sin(phase()*18.0+seed*5.0)*(1.0-smoothstep(0.25,0.50,phase()));
    return bound(index)*(0.7+struggle*0.3);
}
fn effect(p:vec2f)->vec4f { return sampleText(p)*(1.0-power()); }
fn textVertex(instance:u32,corner:vec2f)->SmudgyFragment {
    let index=instance/2u;let thread=instance%2u==1u;
    if power()<0.0001 || (glyphsTruncated() && index>0u) { return SmudgyFragment(vec2f(0),vec2f(0),1,0); }
    let box=column(index);let pad=select(0.0,4.0*text.effect_scale,thread);
    let source=box.xy-vec2f(pad)+(box.zw+vec2f(pad*2.0))*corner;
    let pivot=vec2f(box.x+box.z*0.5,glyphBaseline(index));
    let local=source-pivot;let tight=tension(index)*min(text.effect_scale,2.0)*sqrt(params.intensity);
    let position=pivot+vec2f(local.x*(1.0-tight*0.15)+local.y*tight*0.055,local.y);
    return SmudgyFragment(position,source,1,power());
}
fn textFragment(ink:vec4f,p:vec2f,instance:u32)->vec4f {
    let index=instance/2u;let g=glyphAt(index);let tight=bound(index);let s=text.effect_scale;
    if instance%2u==0u {
        let wrap=(p.y-g.ink.y)/max(g.ink.w,4.0)*2.0+(p.x-g.ink.x)/max(g.ink.z,2.0)*0.40;
        let seam=abs(fract(wrap)-0.5)*max(g.ink.w,4.0)*0.5;
        let pressure=1.0-smoothstep(0.1,1.0*s,seam);
        let native=ink.rgb/max(ink.a,0.001);
        let boundInk=mix(native,params.base.rgb,pressure*tight*0.60);
        return vec4f(boundInk*ink.a,ink.a);
    }
    if any(g.ink.zw<=vec2f(0)) || glyphsTruncated() { return vec4f(0); }
    let box=column(index);
    if any(p<box.xy) || any(p>=box.xy+box.zw) { return vec4f(0); }
    let height=max(g.ink.w,4.0);
    let q=p-g.ink.xy;
    let wrap=q.y/height*2.0+q.x/max(g.ink.z,2.0)*0.40;
    let d=abs(fract(wrap)-0.5)*height*0.5;
    // Only ink and its immediate contour carry the bindings, including the counter edges.
    let hull=max(max(coverage(p+vec2f(0.8*s,0)),coverage(p-vec2f(0.8*s,0))),
        max(coverage(p+vec2f(0,0.8*s)),coverage(p-vec2f(0,0.8*s))));
    let reveal=smoothstep(0.0,0.12,phase()-0.035-g.advance.x/max(text.text_size.x,1.0)*0.28);
    let traced=smoothstep(-0.12,0.03,reveal-q.x/max(g.ink.z,2.0));
    let release=smoothstep(0.77,0.98,phase());
    let broken=select(1.0,smoothstep(0.0,0.12,abs(sin(wrap*3.0+f32(index)))-release),release>0.001);
    let wire=1.0-smoothstep(0.22*s,0.65*s,d);
    let a=wire*traced*broken*hull*tight;
    let gleam=pow(clamp(1.0-d/max(0.65*s,0.01),0.0,1.0),5.0);
    let shade=mix(params.base.rgb,params.bright.rgb,0.65+gleam*0.35);
    return vec4f(mix(shade,params.accent.rgb,gleam*0.3)*a,a);
}

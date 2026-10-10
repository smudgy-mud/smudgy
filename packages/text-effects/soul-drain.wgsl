// The inscription loses substance: holes spread through its ink while ghosted strokes escape.
const TEXT_FRAGMENTS:u32=3u;
struct Parameters { intensity:f32,speed:f32,base:vec4f,bright:vec4f,accent:vec4f }
@group(1) @binding(0) var<uniform> params:Parameters;
fn sh(n:f32)->f32{return fract(sin(n*127.1+text.seed)*43758.5453);}
fn sp()->f32{
    if text.duration<=0.0{return fract(text.time*params.speed/4.1);}
    let rate=clamp(params.speed,0.25,4.0);
    // Bias the action within a finite duration while keeping both endpoints exact.
    return text.progress*rate/(1.0+text.progress*(rate-1.0));
}
fn drain()->f32{
    if params.intensity<=0.0 || params.speed<=0.0{return 0.0;}
    return min(params.intensity,1.5)*smoothstep(0.02,0.57,sp())*(1.0-smoothstep(0.73,0.98,sp()));
}
fn substance(p:vec2f)->f32{
    let q=p/(max(text.text_size.y,16.0)*text.effect_scale);
    let coarse=noise3D(vec3f(q*3.4,text.seed*0.031),0.0);
    let folded=q+vec2f(coarse,noise3D(vec3f(q*2.6,text.seed*0.017+13.0),0.0))*0.23;
    let veins=noise3D(vec3f(folded*4.5,text.seed*0.023+8.0),0.0);
    return clamp(0.50+coarse*0.36+veins*0.22,0.0,1.0);
}
fn remaining(p:vec2f)->f32{
    let threshold=drain()*0.44;
    return smoothstep(threshold-0.035,threshold+0.035,substance(p));
}
fn effect(p:vec2f)->vec4f{
    let source=sampleText(p);let debt=drain();
    if debt<0.0001 || source.a<0.001{return source;}
    let material=substance(p);let threshold=debt*0.44;
    let alive=smoothstep(threshold-0.035,threshold+0.035,material);
    let boundary=exp(-pow((material-threshold)/0.055,2.0))*debt;
    // The surviving ink keeps its styled colour; the wounds are actual transparent holes.
    let cold=mix(source.rgb,params.bright.rgb*source.a,boundary*0.38);
    let bloodless=mix(cold,cold*0.67,debt*0.32);
    return vec4f(bloodless*alive,source.a*alive);
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
fn bounds(index:u32)->vec4f{return column(index/3u);}
fn textVertex(index:u32,corner:vec2f)->SmudgyFragment{
    let debt=drain();
    if debt<0.0001 || (glyphsTruncated() && index/3u>0u){
        return SmudgyFragment(vec2f(0),vec2f(0),1,0);
    }
    let box=bounds(index);let center=box.xy+box.zw*0.5;
    let source=box.xy+corner*box.zw;let seed=sh(f32(index)+2.0);
    let phase=sp();let lag=seed*0.16;
    let t=clamp((phase-0.08-lag)/0.62,0.0,1.0);
    let release=t*t*(3.0-2.0*t);let s=text.effect_scale;
    let outward=vec2f((seed-0.5)*42.0,-9.0-sh(f32(index)+7.0)*24.0);
    let flutter=vec2f(sin(t*4.0+seed*6.283185)-sin(seed*6.283185),
        sin(t*3.141593)*(sh(f32(index)+8.0)-0.5))*6.0;
    let position=center+(outward*release+flutter*t)*s;
    let angle=(seed-0.5)*t*0.40;let depth=1.0+(sh(f32(index)+3.0)-0.5)*t*0.65;
    let local=(source-center)/depth;
    let rotated=vec2f(local.x*cos(angle)-local.y*sin(angle),local.x*sin(angle)+local.y*cos(angle));
    let opacity=smoothstep(0.03,0.20,t)*(1.0-smoothstep(0.64,0.98,t))*0.28;
    return SmudgyFragment(position+rotated,source,depth,opacity*min(params.intensity,1.0));
}
fn textFragment(unused:vec4f,source:vec2f,index:u32)->vec4f{
    let box=bounds(index);
    let phase=sp();let seed=sh(f32(index)+2.0);
    let t=clamp((phase-0.08-seed*0.16)/0.62,0.0,1.0);
    let flow=noise3D(vec3f(source/text.effect_scale*0.09,
        text.time*params.speed*0.24+seed*6.0),0.0);
    let p=source+vec2f(flow,sin(source.y*0.14+seed*6.0))*t*3.2*text.effect_scale;
    let ink=sampleTextLod(p,t*1.2);
    let stolen=1.0-remaining(source);
    let gate=smoothstep(box.x,box.x+0.6,source.x)
        *(1.0-smoothstep(box.x+box.z-0.6,box.x+box.z,source.x));
    let tint=mix(params.bright,params.accent,seed*0.34);
    let light=smoothstep(0.3,0.8,dot(text.background.rgb,vec3f(0.2126,0.7152,0.0722)));
    let color=mix(tint.rgb,params.base.rgb,light*0.75);
    let a=ink.a*stolen*gate*tint.a;
    return vec4f(color*a,a);
}

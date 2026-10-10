// Individual letters resist, then breathe with the same will. Their baseline never travels.
const TEXT_FRAGMENTS:u32=1u;
struct Parameters { intensity:f32,speed:f32,base:vec4f,bright:vec4f,accent:vec4f }
@group(1) @binding(0) var<uniform> params:Parameters;
fn hash(n:f32)->f32{return fract(sin(n*127.1+text.seed)*43758.5453);}
fn phase()->f32{
    if text.duration<=0.0{return fract(text.time*params.speed/4.0);}
    let rate=clamp(params.speed,0.25,4.0);
    return text.progress*rate/(1.0+text.progress*(rate-1.0));
}
fn power()->f32{
    if params.intensity<=0.0 || params.speed<=0.0{return 0.0;}
    return min(params.intensity,1.5)*smoothstep(0.0,0.16,phase())*(1.0-smoothstep(0.82,0.99,phase()));
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
fn will(index:u32)->f32{
    let t=phase();let seed=hash(f32(index)+2.0);
    let free=sin(t*(24.0+seed*16.0)+seed*6.283185);
    let imposed=sin(t*24.0);
    return mix(free,imposed,smoothstep(0.22,0.65,t));
}
fn effect(p:vec2f)->vec4f{
    if power()<0.0001{return sampleText(p);}
    return vec4f(0);
}
fn textVertex(index:u32,corner:vec2f)->SmudgyFragment{
    let strength=power();
    if strength<0.0001 || (glyphsTruncated() && index>0u){return SmudgyFragment(vec2f(0),vec2f(0),1,0);}
    let box=column(index);let padding=vec2f(3.5*text.effect_scale,0);
    let source=box.xy-padding+corner*(box.zw+padding*2.0);
    let center=box.xy+box.zw*0.5;
    let pivot=vec2f(center.x,glyphBaseline(index));
    let beat=will(index)*strength;
    // Small opposing width/height contractions keep the feet of the glyph planted.
    let size=vec2f(1.0+beat*0.14,1.0-beat*0.075);
    let resisting=1.0-smoothstep(0.22,0.65,phase());
    let lean=(hash(f32(index)+7.0)-0.5)*beat*0.22*resisting;
    let local=(source-pivot)*mix(vec2f(1),size,clamp(text.effect_scale,0.25,2.0));
    let position=pivot+vec2f(local.x+local.y*lean,local.y);
    return SmudgyFragment(position,source,1,1);
}
fn textFragment(original:vec4f,p:vec2f,index:u32)->vec4f{
    let strength=power();let box=column(index);let glyph=glyphAt(index);
    let height=max(glyph.advance.w,12.0);
    let y=clamp((p.y-glyph.advance.y)/height,0.0,1.0);
    let bend=will(index)*strength*sin(y*3.141593)*2.2*text.effect_scale;
    let q=p+vec2f(bend,0);
    if q.x<box.x || q.x>box.x+box.z || q.y<box.y || q.y>box.y+box.w{return vec4f(0);}
    let ink=sampleText(q);
    let claimed=smoothstep(0.25,0.70,phase());
    let heartbeat=pow(max(0.0,sin(phase()*24.0)),4.0);
    let tissue=noise3D(vec3f(q/height*7.0,text.seed*0.013),0.0);
    let heat=clamp((claimed*(0.68+heartbeat*0.28)+tissue*0.13)*strength,0.0,1.0);
    let light=smoothstep(0.3,0.8,dot(text.background.rgb,vec3f(0.2126,0.7152,0.0722)));
    let tint=mix(mix(params.bright.rgb,params.accent.rgb,heartbeat*0.34),params.base.rgb,light*0.8);
    return vec4f(mix(ink.rgb,tint*ink.a,heat*0.90),ink.a);
}

// Unreliable readings peel away. One precise, original inscription remains.
const TEXT_FRAGMENTS:u32=2u;
struct Parameters { intensity:f32,speed:f32,base:vec4f,bright:vec4f,accent:vec4f }
@group(1) @binding(0) var<uniform> params:Parameters;
fn hash(n:f32)->f32{return fract(sin(n*127.1+text.seed)*43758.5453);}
fn phase()->f32{
    if text.duration<=0.0{return fract(text.time*params.speed/4.2);}
    let rate=clamp(params.speed,0.25,4.0);
    return text.progress*rate/(1.0+text.progress*(rate-1.0));
}
fn power()->f32{
    if params.intensity<=0.0 || params.speed<=0.0{return 0.0;}
    return min(params.intensity,1.0)*smoothstep(0.0,0.12,phase())*(1.0-smoothstep(0.86,0.99,phase()));
}
fn cleared(x:f32)->f32{
    return smoothstep(0.23+x*0.42,0.32+x*0.42,phase());
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
fn effect(p:vec2f)->vec4f{
    let ink=sampleText(p);let strength=power();
    if strength<0.0001{return ink;}
    let x=clamp(p.x/max(text.text_size.x,1.0),0.0,1.0);let seeing=cleared(x);
    let front=seeing*(1.0-seeing)*4.0;
    let truth=mix(ink.rgb,params.accent.rgb*ink.a,front*0.60*strength);
    // Reduce premultiplied colour and opacity together, on light as well as dark terminals.
    return vec4f(truth,ink.a)*mix(1.0,0.22,(1.0-seeing)*strength);
}
fn textVertex(instance:u32,corner:vec2f)->SmudgyFragment{
    let strength=power();let index=instance/2u;
    if strength<0.0001 || (glyphsTruncated() && index>0u){return SmudgyFragment(vec2f(0),vec2f(0),1,0);}
    let box=column(index);let source=box.xy+corner*box.zw;let center=box.xy+box.zw*0.5;
    let seed=hash(f32(instance)+2.0);let side=select(-1.0,1.0,instance%2u==0u);
    let reveal=cleared(center.x/max(text.text_size.x,1.0));
    let away=smoothstep(0.0,1.0,reveal);let s=text.effect_scale;
    let angle=side*(0.17+seed*0.10+away*0.55)*strength;
    let local=(source-center)*vec2f(1.0+side*0.20*strength,1.0);
    let rotated=vec2f(local.x*cos(angle)-local.y*sin(angle),local.x*sin(angle)+local.y*cos(angle));
    let offset=vec2f(side*(2.3+away*(8.0+seed*7.0)),side*(0.7+away*6.0))*strength*s;
    return SmudgyFragment(center+rotated+offset,source,1.0+away*0.18,
        strength*(1.0-away)*0.70);
}
fn textFragment(original:vec4f,p:vec2f,instance:u32)->vec4f{
    let side=f32(instance%2u);let box=column(instance/2u);
    let warped=p+vec2f(sin((p.y-box.y)/max(box.w,1.0)*6.283185+hash(f32(instance))*6.0),0)*power()*0.8;
    if warped.x<box.x || warped.x>box.x+box.z{return vec4f(0);}
    let ink=sampleText(warped);
    let light=smoothstep(0.3,0.8,dot(text.background.rgb,vec3f(0.2126,0.7152,0.0722)));
    let tint=mix(mix(params.base.rgb,params.bright.rgb,side),params.base.rgb,light*0.65);
    return vec4f(tint*ink.a,ink.a);
}

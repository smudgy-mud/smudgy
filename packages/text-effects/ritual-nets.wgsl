// A binding tangles the ink into tight ligatures. A counterspell arrests a cipher forming from that ink.
const TEXT_FRAGMENTS:u32=12u;
struct Parameters { intensity:f32,speed:f32,variant:f32,
    base:vec4f,bright:vec4f,accent:vec4f }
@group(1) @binding(0) var<uniform> params:Parameters;
fn nh(n:f32)->f32{return fract(sin(n*127.1+text.seed)*43758.5453);}
fn np()->f32{
    if text.duration<=0.0{return fract(text.time*params.speed/4.2);}
    let rate=clamp(params.speed,0.25,4.0);
    // Bias the action within a finite duration while keeping both endpoints exact.
    return text.progress*rate/(1.0+text.progress*(rate-1.0));
}
fn power()->f32{
    if params.intensity<=0.0 || params.speed<=0.0{return 0.0;}
    return min(params.intensity,1.2)*smoothstep(0.02,0.42,np())*(1.0-smoothstep(0.64,0.98,np()));
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
fn cipher(instance:u32)->f32{
    let location=f32(instance/12u)/max(f32(glyphCount()-1u),1.0);
    let formed=smoothstep(0.05+location*0.23,0.27+location*0.23,np());
    let repaired=smoothstep(0.35+location*0.34,0.48+location*0.34,np());
    return formed*(1.0-repaired)*min(params.intensity,1.0);
}
fn effect(p:vec2f)->vec4f{
    if power()<0.0001{return sampleText(p);}
    return vec4f(0);
}
fn turn(p:vec2f,a:f32)->vec2f{return vec2f(p.x*cos(a)-p.y*sin(a),p.x*sin(a)+p.y*cos(a));}
fn textVertex(instance:u32,corner:vec2f)->SmudgyFragment{
    let knot=power();
    if knot<0.0001 || (glyphsTruncated() && instance/12u>0u){
        return SmudgyFragment(vec2f(0),vec2f(0),1,0);
    }
    let box=bounds(instance);let source=box.xy+corner*box.zw;
    let center=box.xy+box.zw*0.5;
    let seed=nh(f32(instance/12u)+4.0);let s=text.effect_scale;
    let pivot=vec2f(center.x,glyphBaseline(instance/12u));
    var position=source;var depth=1.0;
    if params.variant<0.5{
        // The inscription itself becomes a knot. Every loop is captured lettering.
        let strain=min(knot,1.0);
        let center=vec2f(text.text_size.x*0.5,pivot.y-text.text_size.y*0.34);
        let theta=source.x/max(text.text_size.x,1.0)*6.283185;
        let radius=min(text.text_size.x*0.068,text.text_size.y*0.60)*s;
        let radius3=2.0+cos(theta*3.0);
        let bound=center+vec2f(radius3*sin(theta*2.0),radius3*cos(theta*2.0)*0.5)*radius;
        let tangent=vec2f(2.0*cos(theta*2.0)*radius3-3.0*sin(theta*2.0)*sin(theta*3.0),
            (-2.0*sin(theta*2.0)*radius3-3.0*cos(theta*2.0)*sin(theta*3.0))*0.5);
        let normal=normalize(vec2f(-tangent.y,tangent.x));
        depth=1.0+sin(theta*3.0)*strain*0.28;
        let ribbon=bound+normal*(source.y-center.y)*0.46;
        position=mix(source,ribbon,strain);
    }else{
        let wrong=cipher(instance);let glyph=glyphAt(instance/12u);
        let glyphCenter=glyph.ink.xy+glyph.ink.zw*0.5;
        let local=source-glyphCenter;
        let angle=select(-1.0,1.0,seed>0.5)*wrong*1.30*min(s,1.7);
        let shear=vec2f(local.x*(1.0-wrong*1.60)+local.y*(seed-0.5)*wrong*0.35,local.y);
        position=glyphCenter+turn(shear,angle);
        depth=1.0+wrong*0.15;
    }
    return SmudgyFragment(position,source,depth,1.0);
}
fn textFragment(original:vec4f,p:vec2f,instance:u32)->vec4f{
    let knot=power();let ink=original;
    if params.variant<0.5{
        let theta=p.x/max(text.text_size.x,1.0)*6.283185;
        let tilt=0.66+0.34*(0.5+0.5*sin(theta*3.0));
        let tint=mix(ink.rgb,params.accent.rgb*ink.a,knot*0.07);
        return vec4f(tint*mix(1.0,tilt,knot),ink.a);
    }
    let wrong=cipher(instance);
    let location=f32(instance/12u)/max(f32(glyphCount()-1u),1.0);
    let front=smoothstep(0.35,0.82,np());
    let unwriting=exp(-pow((location-front)/0.08,2.0));
    let ash=mix(ink.rgb,params.base.rgb*ink.a,wrong*0.36);
    let cleared=mix(ash,params.accent.rgb*ink.a,unwriting*0.24*wrong);
    return vec4f(cleared,ink.a);
}

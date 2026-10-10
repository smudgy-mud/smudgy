// Local weight, pressure, and edge-on transitions operate on the same shaped ink.
const TEXT_FRAGMENTS:u32=1u;
struct Parameters { intensity:f32,speed:f32,variant:f32,amplitude:f32,
    base:vec4f,bright:vec4f,accent:vec4f }
@group(1) @binding(0) var<uniform> params:Parameters;
fn pp()->f32 {
    if text.duration<=0.0{return fract(text.time*params.speed/3.2);}
    let r=clamp(params.speed,0.25,4.0);
    return text.progress*r/(1.0+text.progress*(r-1.0));
}
fn quiet()->bool{return params.intensity<=0.0 || params.speed<=0.0 || params.amplitude<=0.0 || glyphsTruncated();}
fn wave(x:f32)->f32 {
    return exp(-pow((x-(pp()*1.6-0.30))/0.19,2.0))*sin(clamp(pp(),0.0,1.0)*3.141593);
}
fn vanished(x:f32)->f32 {
    let t=pp();
    if params.variant<2.5{return 1.0-smoothstep(x*0.12,0.80+x*0.12,t);}
    if params.variant<3.5{return smoothstep(0.04+x*0.12,0.82+x*0.12,t);}
    let cycle=select(t*2.0,(1.0-t)*2.0,t>0.5);
    return smoothstep(0.04+x*0.12,0.92,cycle);
}
fn rowBaseline(y:f32)->f32 {
    // A normal terminal line exits on its first glyph. Styled multiline widgets
    // can have different line metrics, so use the captured line rectangles.
    for(var i=0u;i<glyphCount();i++){
        let g=glyphAt(i);
        if y>=g.advance.y && y<g.advance.y+g.advance.w{return glyphBaseline(i);}
    }
    return textBaseline();
}
fn effect(p:vec2f)->vec4f {
    let original=sampleText(p);
    if quiet(){return original;}
    if params.variant>1.5 {
        let gone=vanished(0.5);
        if gone<0.0001{return original;}
        let seam=vec2f(text.text_size.x*0.5,textBaseline()-text.text_size.y*0.27);
        let d=abs(p-seam);
        let a=exp(-pow(d.x/(0.45*text.effect_scale),2.0))
            *(1.0-smoothstep(text.text_size.y*0.16,text.text_size.y*0.36,d.y))
            *max(sin(gone*3.141593),0.0)*0.3*params.accent.a;
        return vec4f(params.accent.rgb*a,a);
    }
    if pp()>=0.995{return original;}
    if params.variant>0.5 {
        let x=p.x/max(text.text_size.x,1.0);
        let center=pp()*1.6-0.30;
        let beat=wave(x)*min(params.intensity*params.amplitude*text.effect_scale,2.0);
        let baseline=rowBaseline(p.y);
        let y=p.y-baseline;
        let source=vec2f(p.x+(x-center)*beat*text.text_size.y*4.0+y*beat*0.28,
            baseline+y/(1.0+beat*0.11));
        return sampleText(source);
    }
    let beat=wave(p.x/max(text.text_size.x,1.0));
    let radius=beat*params.intensity*params.amplitude*text.effect_scale*0.95;
    var ink=original;
    for(var j=0;j<8;j++){
        let angle=f32(j)*0.7853982;
        let sample=sampleText(p+vec2f(cos(angle),sin(angle))*radius);
        if sample.a>ink.a{ink=sample;}
    }
    return ink;
}
fn textVertex(instance:u32,corner:vec2f)->SmudgyFragment {
    if quiet() || params.variant<1.5 {
        return SmudgyFragment(vec2f(0),vec2f(0),1.0,0.0);
    }
    let g=glyphAt(instance);
    var lo=g.advance.xy-vec2f(0,2);
    var hi=g.advance.xy+g.advance.zw+vec2f(0,2);
    let first=instance==0u || glyphAt(instance-1u).cluster.z!=g.cluster.z;
    let last=instance+1u==glyphCount() || glyphAt(instance+1u).cluster.z!=g.cluster.z;
    if !last{hi.x=glyphAt(instance+1u).advance.x;}
    if first{lo.x=min(lo.x,g.ink.x)-2.0;}
    if last{hi.x=max(hi.x,g.ink.x+g.ink.z)+2.0;}
    let source=mix(lo,hi,corner);
    let pivot=vec2f((lo.x+hi.x)*0.5,glyphBaseline(instance));
    let x=pivot.x/max(text.text_size.x,1.0);
    let gone=vanished(x);
    if vanished(0.5)<0.0001{return SmudgyFragment(vec2f(0),vec2f(0),1.0,0.0);}
    let v=source-pivot;
    let turn=pow(gone,1.0/clamp(params.amplitude*params.intensity*text.effect_scale,0.25,4.0));
    let squeeze=cos(turn*1.570796);
    let pivotX=mix(pivot.x,text.text_size.x*0.5,pow(gone,1.6));
    let position=vec2f(pivotX+v.x*max(squeeze,0.002),pivot.y+v.y*(1.0-gone*0.18*min(text.effect_scale,2.0)));
    let opacity=1.0-smoothstep(0.72,0.99,gone);
    return SmudgyFragment(position,source,1.0,opacity);
}
fn textFragment(color:vec4f,source:vec2f,instance:u32)->vec4f {
    return color;
}

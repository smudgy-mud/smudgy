// The inscription is tessellated into curved ink ribbons and consumed by a dark aperture.
const TEXT_FRAGMENTS:u32=32u;
struct Parameters { intensity:f32,speed:f32,
    base:vec4f,bright:vec4f,accent:vec4f }
@group(1) @binding(0) var<uniform> params:Parameters;
fn hp()->f32 {
    if text.duration<=0.0{return fract(text.time*params.speed/5.2);}
    let r=clamp(params.speed,0.25,4.0);
    return text.progress*r/(1.0+text.progress*(r-1.0));
}
fn ease(a:f32,b:f32,x:f32)->f32 {
    let t=clamp((x-a)/(b-a),0.0,1.0);return t*t*t*(t*(t*6.0-15.0)+10.0);
}
fn energy()->f32 {
    if params.intensity<=0.0 || params.speed<=0.0{return 0.0;}
    return ease(0.0,0.15,hp())*(1.0-ease(0.80,0.995,hp()))*min(params.intensity,1.0)
        *max(params.base.a,max(params.bright.a,params.accent.a));
}
fn radius()->f32{return min(52.0*text.effect_scale*sqrt(max(params.intensity,0.01)),min(text.surface.x,text.surface.y)*0.22);}
fn center()->vec2f {
    let margin=vec2f(radius()*2.1,radius()*1.45);
    return clamp(vec2f(text.text_size.x*0.5,textBaseline()-text.text_size.y*0.3),
        text.paint_offset+margin,text.paint_offset+text.surface-margin);
}
fn rh(n:f32)->f32{return fract(sin(n*127.1+text.seed)*43758.5453);}
fn effect(p:vec2f)->vec4f {
    let power=energy();
    if power<0.0001 || glyphsTruncated(){return sampleText(p);}
    let t=hp();let q=p-center();let r=radius()*ease(0.03,0.30,t);
    let distance=length(q);
    if distance>radius()*2.7{return vec4f(0);}
    let angle=atan2(q.y,q.x);
    let flow=noise3D(vec3f(angle*2.0,distance/(6.0*text.effect_scale)-t*5.0,3.0),0.0)*0.5+0.5;
    let edge=abs(distance-r);
    let strength=sqrt(params.intensity);
    let photon=exp(-pow(edge/(1.0*text.effect_scale*strength),2.0));
    let haze=exp(-edge/(14.0*text.effect_scale*strength))*0.22;
    let lens=exp(-pow((distance-r*1.11)/(2.0*text.effect_scale*strength),2.0))*flow*0.45;
    let orbit=length(q/vec2f(1.0,0.28));
    let eddy=noise3D(vec3f(q/(11.0*text.effect_scale)+vec2f(flow*2.0),t*3.0),0.0)*0.5+0.5;
    let disk=exp(-pow((orbit-r*1.55)/(22.0*text.effect_scale*strength),2.0));
    let grooves=pow(0.5+0.5*sin(orbit/(2.1*text.effect_scale)+eddy*7.0-t*18.0),2.0);
    let doppler=0.45+0.55*smoothstep(-r*1.5,r*1.5,q.x);
    let arch=exp(-pow((distance-r*1.12)/(8.0*text.effect_scale),2.0))
        *(1.0-smoothstep(-r*0.05,r*0.28,q.y))*(0.45+flow*0.55);
    let diskA=disk*(0.50+grooves*0.50)*doppler+arch*0.65;
    let tint=mix(params.base,params.bright,clamp(flow*0.65+diskA,0.0,1.0));
    let shade=mix(tint,params.accent,clamp(photon*0.95+diskA*0.25,0.0,1.0));
    let hole=(1.0-smoothstep(r-1.0,r+1.0,distance))*power*0.995;
    let foregroundDisk=diskA*select(0.0,1.0,q.y>0.0);
    let glow=clamp(photon*0.55+haze+lens+diskA*(1.0-hole)+foregroundDisk*hole,0.0,0.98)*power*shade.a;
    let alpha=hole+glow*(1.0-hole);
    let rgb=vec3f(0.003,0.002,0.007)*hole*(1.0-glow)+shade.rgb*glow;
    return vec4f(min(rgb,vec3f(max(alpha,glow))),max(alpha,glow));
}
fn textVertex(instance:u32,corner:vec2f)->SmudgyFragment {
    if energy()<0.0001 || glyphsTruncated(){return SmudgyFragment(vec2f(0),vec2f(0),1,0);}
    let index=instance/32u;let piece=instance%16u;let trail=instance%32u>=16u;
    let g=glyphAt(index);
    var lo=g.advance.xy-vec2f(0,2);var hi=g.advance.xy+g.advance.zw+vec2f(0,2);
    let first=index==0u || glyphAt(index-1u).cluster.z!=g.cluster.z;
    let last=index+1u==glyphCount() || glyphAt(index+1u).cluster.z!=g.cluster.z;
    if !last{hi.x=glyphAt(index+1u).advance.x;}
    if first{lo.x=min(lo.x,g.ink.x)-2.0;}
    if last{hi.x=max(hi.x,g.ink.x+g.ink.z)+2.0;}
    let tile=vec2f(f32(piece%8u),f32(piece/8u));
    let source=lo+(tile+corner)*(hi-lo)/vec2f(8,2);
    let tileCenter=lo+(tile+vec2f(0.5))*(hi-lo)/vec2f(8,2);
    let seed=rh(f32(index)+7.0);
    let t=max(hp()-select(0.0,0.018,trail),0.0);
    let depart=ease(0.08+seed*0.10,0.49+seed*0.06,t);
    let restore=ease(0.66+seed*0.04,0.995,t);
    let flight=depart*(1.0-restore)*min(params.intensity,1.0);
    let v=tileCenter-center();
    let distance=max(length(v),0.01);
    let angle=atan2(v.y,v.x)+flight*(3.6+t*6.0);
    let collapse=1.0-ease(0.27,0.63,t)*(1.0-ease(0.67,0.99,t));
    let radial=distance*pow(1.0-flight,1.15)+radius()*0.85*flight*collapse;
    let destination=center()+vec2f(cos(angle),sin(angle))*radial*vec2f(1.0,1.0-flight*0.42);
    let local=source-tileCenter;
    let tangent=vec2f(-sin(angle),cos(angle));
    let normal=vec2f(cos(angle),sin(angle));
    let stretched=tangent*local.x*(1.0+flight*9.0*sqrt(params.intensity))+normal*local.y*(1.0-flight*0.72);
    let position=mix(source,destination+stretched,flight);
    let swallowed=ease(0.44,0.64,t)*(1.0-ease(0.67,0.90,t));
    let opacity=(1.0-swallowed)*select(1.0,flight*0.27,trail);
    return SmudgyFragment(position,source,1.0,opacity);
}
fn textFragment(ink:vec4f,p:vec2f,instance:u32)->vec4f {
    let tint=ease(0.12,0.48,hp())*(1.0-ease(0.72,0.99,hp()))*energy()*params.accent.a;
    let colour=mix(ink.rgb,params.accent.rgb*ink.a,tint*0.72);
    return vec4f(colour,ink.a);
}

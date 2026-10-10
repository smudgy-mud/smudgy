// Fixed mineral grain and faults grow through the actual lettering. No drifting texture.
struct Parameters { intensity:f32, speed:f32, base:vec4f, bright:vec4f, accent:vec4f }
@group(1) @binding(0) var<uniform> params:Parameters;
fn hash(p:vec2f)->vec2f {
    return fract(sin(vec2f(dot(p,vec2f(127.1,311.7)),dot(p,vec2f(269.5,183.3)))+text.seed)*43758.5453);
}
fn mineral(p:vec2f)->vec2f {
    let cell=floor(p); let local=fract(p);
    var first=8.0; var second=8.0; var grain=0.0;
    for(var y=-1;y<=1;y++) { for(var x=-1;x<=1;x++) {
        let offset=vec2f(f32(x),f32(y)); let seed=hash(cell+offset);
        let d=length(offset+seed*0.8+0.1-local);
        if d<first { second=first;first=d;grain=seed.x; }
        else { second=min(second,d); }
    }}
    return vec2f(second-first,grain);
}
fn phase()->f32 {
    if text.duration<=0.0 { return fract(text.time*params.speed/4.0); }
    let rate=clamp(params.speed,0.25,4.0);
    return text.progress*rate/(1.0+text.progress*(rate-1.0));
}
fn effect(p:vec2f)->vec4f {
    let original=sampleText(p);
    if params.intensity<=0.0 || params.speed<=0.0 { return original; }
    let t=phase(); let s=text.effect_scale;
    let q=p/max(s,0.25);
    let grain=mineral(q/11.0);
    let coarse=noise3D(vec3f(q*0.15,text.seed*0.01),0.0);
    let x=p.x/max(text.text_size.x,1.0);
    let onset=0.07+x*0.32+coarse*0.04;
    let formation=smoothstep(onset,onset+0.13,t);
    let release=1.0-smoothstep(0.81,0.995,t);
    let strength=formation*release*min(params.intensity,1.0);
    // Compression is about the true baseline, not the line box center.
    let source=vec2f(p.x,textBaseline()+(p.y-textBaseline())/(1.0-0.025*strength));
    let ink=sampleText(source);
    let radius=0.65*strength*s*sqrt(params.intensity);
    let dx=coverage(source+vec2f(0.75,0))-coverage(source-vec2f(0.75,0));
    let dy=coverage(source+vec2f(0,0.75))-coverage(source-vec2f(0,0.75));
    let body=max(ink.a,max(max(coverage(source+vec2f(radius,0)),coverage(source-vec2f(radius,0))),
        max(coverage(source+vec2f(0,radius)),coverage(source-vec2f(0,radius)))));
    let depth=coverage(source-vec2f(0.8,0.8)*s)*strength;
    let relief=clamp(-dx*0.26-dy*0.40,-0.5,0.5);
    let rough=noise3D(vec3f(q*0.8,19.0),0.0);
    let value=clamp(0.68+grain.y*0.22+coarse*0.13+rough*0.05+relief,0.0,1.0);
    var stone=mix(params.base.rgb,params.bright.rgb,value);
    let crack=1.0-smoothstep(0.015,0.065,grain.x);
    let rim=exp(-pow((grain.x-0.085)/0.035,2.0));
    stone=mix(stone,params.base.rgb*0.55,crack*0.54);
    stone=mix(stone,params.accent.rgb,rim*0.18);
    // Brief pale mineral deposits lead the heavy, quiet final surface.
    let front=formation*(1.0-formation)*4.0;
    stone=mix(stone,params.accent.rgb,front*0.48);
    let alpha=body+depth*(1.0-body);
    let colour=stone*body+params.base.rgb*depth*(1.0-body);
    return mix(original,vec4f(colour,alpha),strength);
}

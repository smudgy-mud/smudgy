// Darkness is a loss of the inscription itself: inward tension, hollowed strokes, and recovery.
struct Parameters { intensity:f32,speed:f32,base:vec4f,bright:vec4f,accent:vec4f }
@group(1) @binding(0) var<uniform> params:Parameters;
fn phase()->f32{
    if text.duration<=0.0{return fract(text.time*params.speed/4.2);}
    let rate=clamp(params.speed,0.25,4.0);
    // Bias the action within a finite duration while keeping both endpoints exact.
    return text.progress*rate/(1.0+text.progress*(rate-1.0));
}
fn absence()->f32{
    if params.intensity<=0.0 || params.speed<=0.0{return 0.0;}
    return smoothstep(0.03,0.63,phase())*(1.0-smoothstep(0.76,0.98,phase()))*min(params.intensity,1.3);
}
fn tissue(p:vec2f)->f32{
    let q=p/(max(text.text_size.y,16.0)*text.effect_scale);
    let a=noise3D(vec3f(q*3.2,text.seed*0.019),0.0);
    let b=noise3D(vec3f(q*8.0+vec2f(a*1.2),text.seed*0.033+7.0),0.0);
    return 0.50+a*0.36+b*0.19;
}
fn effect(p:vec2f)->vec4f{
    let debt=absence();if debt<0.0001{return sampleText(p);}
    let s=text.effect_scale;let baseline=textBaseline();
    let center=vec2f(text.text_size.x*0.5,baseline-text.text_size.y*0.34);
    let relative=p-center;
    let locality=exp(-pow(relative.x/max(text.text_size.x*0.43,1.0),2.0));
    let tension=debt*locality;
    let rough=noise3D(vec3f(p/s*0.045,text.seed*0.023),0.0);
    let cross=noise3D(vec3f(p/s*0.083+vec2f(rough*1.7),text.seed*0.039+11.0),0.0);
    let at=vec2f(center.x+relative.x*(1.0+tension*0.82),
        baseline+(p.y-baseline)*(1.0+tension*0.12))
        +vec2f(cross*3.0,rough*2.4)*tension*s;
    let source=sampleText(at);
    let location=abs(at.x-center.x)/max(text.text_size.x*0.5,1.0);
    let reached=smoothstep(location-0.10,location+0.20,debt);
    let substance=tissue(at);
    let threshold=reached*debt*0.68;
    let survives=smoothstep(threshold-0.025,threshold+0.025,substance);
    let rim=exp(-pow((substance-threshold)/0.025,2.0))*debt*0.17;
    let depleted=source*survives;
    let color=mix(depleted.rgb,depleted.rgb*0.45,debt*reached*0.55);
    // The narrow boundary is captured ink. There is no portal, ring, or painted oval.
    let edge=mix(source.rgb,params.bright.rgb*source.a,0.65);
    return vec4f(color+edge*rim*survives,depleted.a);
}

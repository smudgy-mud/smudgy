// Venom spreads under the skin of the inscription. Veins stay attached to the source strokes.
struct Parameters { intensity:f32,speed:f32,base:vec4f,bright:vec4f,accent:vec4f }
@group(1) @binding(0) var<uniform> params:Parameters;
fn phase()->f32 {
    if text.duration<=0.0 { return fract(text.time*params.speed/4.4); }
    let rate=clamp(params.speed,0.25,4.0);
    return text.progress*rate/(1.0+text.progress*(rate-1.0));
}
fn effect(p:vec2f)->vec4f {
    let native=sampleText(p);
    if params.intensity<=0.0 || params.speed<=0.0 { return native; }
    let t=phase();let s=text.effect_scale;let q=p/max(s,0.25);
    let seed=text.seed*0.017;
    let broad=noise3D(vec3f(q*vec2f(0.055,0.11),seed),0.0);
    let distorted=q+vec2f(broad*5.0,noise3D(vec3f(q*0.08,seed+9.0),0.0)*3.0);
    let veins=noise3D(vec3f(distorted*vec2f(0.17,0.23),seed+17.0),0.0);
    let small=noise3D(vec3f(distorted*0.53,seed+31.0),0.0);
    let x=p.x/max(text.text_size.x,1.0);
    let infection=0.09+x*0.25+broad*0.10;
    let reached=smoothstep(infection,infection+0.15,t);
    let power=reached*min(params.intensity,1.0)*(1.0-smoothstep(0.80,0.995,t));
    let channels=1.0-smoothstep(0.015,0.105,abs(veins+small*0.16));
    let pulse=0.5+0.5*sin(t*18.0+broad*4.0);
    let blister=smoothstep(0.14,0.52,broad+veins*0.25);
    let swelling=vec2f(sin(broad*7.0),cos(veins*5.0))*blister*power*(0.55+pulse*0.55)*s*sqrt(params.intensity);
    let ink=sampleText(p+swelling);
    let dx=coverage(p+vec2f(0.6,0))-coverage(p-vec2f(0.6,0));
    let dy=coverage(p+vec2f(0,0.6))-coverage(p-vec2f(0,0.6));
    let edge=clamp(abs(dx)+abs(dy),0.0,1.0);
    // A quiet bruised body, with wet, narrow veins rather than luminous moving noise.
    let lightBackground=smoothstep(0.3,0.8,dot(text.background.rgb,vec3f(0.2126,0.7152,0.0722)));
    let body=params.base.rgb*(1.12+edge*0.20+blister*0.10);
    var venom=mix(body,params.bright.rgb,channels*(0.48+pulse*0.30));
    let rim=exp(-pow((abs(veins+small*0.16)-0.12)/0.05,2.0));
    venom=mix(venom,params.bright.rgb,rim*(0.20+pulse*0.18));
    venom=mix(venom,params.accent.rgb,blister*edge*pulse*0.20);
    venom*=1.0-lightBackground*0.18;
    return mix(native,vec4f(venom*ink.a,ink.a),power);
}

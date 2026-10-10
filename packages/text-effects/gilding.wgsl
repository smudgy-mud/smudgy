// Gold leaf adheres to the source ink. Creases flatten before one polished reflection.
struct Parameters { intensity:f32, speed:f32, base:vec4f, bright:vec4f, accent:vec4f }
@group(1) @binding(0) var<uniform> params:Parameters;
fn phase()->f32 {
    if text.duration<=0.0 { return fract(text.time*params.speed/3.6); }
    let rate=clamp(params.speed,0.25,4.0);
    return text.progress*rate/(1.0+text.progress*(rate-1.0));
}
fn effect(p:vec2f)->vec4f {
    let ink=sampleText(p);
    if ink.a<0.001 || params.intensity<=0.0 || params.speed<=0.0 { return ink; }
    let t=phase(); let s=text.effect_scale;
    let q=p/max(s,0.25);
    // Static folds belong to the material; only their relief settles over time.
    let broad=noise3D(vec3f(q*0.095,text.seed*0.01),0.0);
    let fine=noise3D(vec3f(q*0.36,7.0+text.seed*0.01),0.0);
    let x=p.x/max(text.text_size.x,1.0);
    let arrival=0.10+x*0.46+broad*0.035+fine*0.013;
    let leaf=smoothstep(arrival,arrival+0.055,t);
    let local=smoothstep(arrival+0.035,arrival+0.22,t);
    let release=1.0-smoothstep(0.86,0.995,t);
    let strength=min(params.intensity,1.0)*leaf*release;
    let relief=(broad*0.65+fine*0.35)*(1.0-local)*0.28*params.intensity;
    let foil=abs(sin(q.x*0.53+q.y*0.91+broad*3.6+fine*1.5));
    let crease=pow(1.0-foil,7.0)*(1.0-local);
    // Ink gradients make the same bevel follow every font, including italic and ligatures.
    let dx=coverage(p+vec2f(0.55,0))-coverage(p-vec2f(0.55,0));
    let dy=coverage(p+vec2f(0,0.55))-coverage(p-vec2f(0,0.55));
    let bevel=clamp(-dx*0.4-dy*0.65,-0.65,0.65);
    let height=max(textBaseline(),1.0);
    let bands=0.76+0.24*sin(p.y/height*11.0+0.6)+relief+bevel*0.35;
    var gold=mix(params.base.rgb,params.bright.rgb,clamp(bands,0.0,1.0));
    gold*=1.0-crease*0.34;
    let sweep=(t-0.48)/0.34;
    let distance=x-sweep+(p.y-height*0.5)/max(text.text_size.x,1.0)*0.32;
    let reflection=exp(-distance*distance/0.0012)*smoothstep(0.46,0.52,t)*(1.0-smoothstep(0.80,0.86,t));
    let edge=clamp(abs(dx)+abs(dy),0.0,1.0);
    gold=mix(gold,params.accent.rgb,clamp(reflection*(0.72+edge*0.28)*params.intensity,0.0,1.0));
    return vec4f(mix(ink.rgb,gold*ink.a,strength),ink.a);
}

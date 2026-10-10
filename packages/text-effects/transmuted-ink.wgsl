// Optical, molten and corroded ink share a fixed, text-local fracture field.
// No captured background or per-frame script work is required.
struct Parameters { intensity:f32, speed:f32, variant:f32,
    base:vec4f, bright:vec4f, accent:vec4f }
@group(1) @binding(0) var<uniform> params:Parameters;
fn mh(p:vec2f)->vec2f {
    return fract(sin(vec2f(dot(p,vec2f(127.1,311.7)),dot(p,vec2f(269.5,183.3)))+text.seed)*43758.5453);
}
fn facets(p:vec2f)->vec4f {
    let cell=floor(p);let local=fract(p);
    var a=20.0;var b=20.0;var seed=vec2f(0);var delta=vec2f(0);
    for(var y=-1;y<=1;y++){for(var x=-1;x<=1;x++){
        let o=vec2f(f32(x),f32(y));let h=mh(cell+o);
        let v=o+h*0.78+0.11-local;let d=dot(v,v);
        if d<a {b=a;a=d;seed=h;delta=v;} else {b=min(b,d);}
    }}
    return vec4f(sqrt(b)-sqrt(a),seed.x,delta);
}
fn mp()->f32 {
    if text.duration<=0.0{return fract(text.time*params.speed/3.8);}
    let r=clamp(params.speed,0.25,4.0);
    return text.progress*r/(1.0+text.progress*(r-1.0));
}
fn contrast(ink:vec4f)->vec4f {
    let light=smoothstep(0.3,0.8,dot(text.background.rgb,vec3f(0.2126,0.7152,0.0722)));
    return vec4f(ink.rgb*mix(1.0,0.42,light),ink.a);
}
fn effect(p:vec2f)->vec4f {
    let original=sampleText(p);
    if params.intensity<=0.0 || params.speed<=0.0{return original;}
    let t=mp();let s=max(text.effect_scale,0.25);
    let q=p/s;let f=facets(q/7.0);
    let along=clamp(p.x/max(text.text_size.x,1.0),0.0,1.0);
    let strength=smoothstep(0.02+along*0.20,0.20+along*0.20,t)
        *(1.0-smoothstep(0.80,0.995,t))*min(params.intensity,1.0)
        *max(params.base.a,max(params.bright.a,params.accent.a));
    if strength<=0.001{return original;}
    if params.variant>1.5 {
        let age=smoothstep(0.10,0.72,t)*min(params.intensity,1.4);
        let grain=noise3D(vec3f(q*0.46,text.seed*0.03),0.0)*0.5+0.5;
        let fine=noise3D(vec3f(q*1.1,7.0),0.0)*0.5+0.5;
        let inner=min(min(coverage(p+vec2f(0.65,0)),coverage(p-vec2f(0.65,0))),
            min(coverage(p+vec2f(0,0.65)),coverage(p-vec2f(0,0.65))));
        let exposed=clamp(original.a-inner,0.0,1.0);
        let damage=age*(0.20+exposed*0.56+grain*0.43);
        let pits=smoothstep(0.38,0.62,damage-fine*0.3);
        let body=original.a*(1.0-pits*0.88);
        let rust=mix(params.base,params.bright,clamp(grain*0.74+fine*0.2,0.0,1.0));
        let lip=clamp(exposed*0.42+pits*0.38,0.0,1.0);
        let colour=mix(rust,params.accent,lip);
        let tarnish=1.0-max(params.intensity-1.0,0.0)*0.32;
        var result=vec4f(colour.rgb*body*colour.a*tarnish,body*colour.a);
        // Each chip is a small piece of the captured inscription. Most is dust;
        // occasional larger flakes expose their original curved ink edge.
        for(var j=0u;j<18u;j++){
            if glyphCount()==0u{break;}
            let g=glyphAt(min(u32(f32(glyphCount())*(f32(j)+0.5)/18.0),glyphCount()-1u));
            let h=mh(vec2f(f32(j),13.0));
            let center=g.ink.xy+g.ink.zw*vec2f(0.22+h.x*0.56,0.25+h.y*0.5);
            let local=max(t-0.18-h.y*0.30,0.0);
            let life=smoothstep(0.0,0.07,local)*(1.0-smoothstep(0.22,0.48,local));
            let drift=vec2f((h.x-0.5)*16.0,local*35.0)*local*s;
            let d=(p-center-drift)/s;
            let angle=local*(h.x-0.5)*9.0;
            let v=vec2f(d.x*cos(angle)-d.y*sin(angle),d.x*sin(angle)+d.y*cos(angle));
            let chip=(1.0-smoothstep(0.7,1.65+h.x,v.x*v.x+v.y*v.y))*coverage(center+v*s)*life*age;
            let a=clamp(chip*params.bright.a,0.0,1.0);
            result=vec4f(result.rgb*(1.0-a)+params.bright.rgb*a,result.a+(1.0-result.a)*a);
        }
        return mix(original,contrast(result),strength);
    }
    if params.variant>0.5 {
        let rock=facets(q/8.0);
        let churn=noise3D(vec3f(q*0.19,t*5.0),0.0)*0.5+0.5;
        let heat=clamp(smoothstep(0.08+along*0.24,0.22+along*0.24,t)
            *(1.0-smoothstep(0.36+along*0.25,0.75+along*0.16,t))*sqrt(params.intensity),0.0,1.0);
        let fissure=1.0-smoothstep(0.035,0.16+heat*0.12,rock.x);
        let molten=clamp(fissure*(0.28+churn*0.45)*(0.25+heat*0.75)+heat*0.76,0.0,1.0);
        let rim=exp(-pow((rock.x-0.14)/0.055,2.0));
        let relief=coverage(p-vec2f(0.6,0.6))-coverage(p+vec2f(0.6,0.6));
        let crust=params.base.rgb*(0.72+rock.y*0.9)+vec3f(max(relief,0.0)*0.2);
        let hot=mix(params.bright,params.accent,pow(molten,2.0));
        let colour=mix(crust,hot.rgb,molten)*(1.0-rim*0.18);
        let rgba=mix(params.base,hot,molten);
        let edge=max(max(coverage(p+vec2f(s,0)),coverage(p-vec2f(s,0))),
            max(coverage(p+vec2f(0,s)),coverage(p-vec2f(0,s))));
        let ember=max(edge-original.a,0.0)*heat*0.65*params.bright.a;
        let alpha=clamp(original.a*rgba.a+ember,0.0,1.0);
        let rgb=colour*original.a*rgba.a+params.bright.rgb*ember;
        return mix(original,contrast(vec4f(min(rgb,vec3f(alpha)),alpha)),strength);
    }
    // A facet refracts the captured ink by a fraction of a pixel. Separate
    // wavelength samples remain inside the letter silhouette, never a drop shadow.
    let shift=f.zw*0.95*s*strength*sqrt(params.intensity);
    let a=coverage(p+shift);let red=coverage(p+shift+vec2f(0.55,0.12)*s);
    let blue=coverage(p+shift-vec2f(0.55,0.12)*s);
    let dx=coverage(p+vec2f(0.6,0))-coverage(p-vec2f(0.6,0));
    let dy=coverage(p+vec2f(0,0.6))-coverage(p-vec2f(0,0.6));
    let edge=clamp(abs(dx)+abs(dy),0.0,1.0);
    let plane=dot(q,normalize(vec2f(0.75+f.y,-0.55)))+f.y*12.0;
    let sweep=(t-0.16)*max(text.text_size.x/s,30.0)*1.55;
    let gleam=exp(-pow((plane-sweep)/2.0,2.0));
    let echo=exp(-pow((plane-sweep+13.0)/2.1,2.0))*0.27;
    let fracture=(1.0-smoothstep(0.012,0.075,f.x));
    let rim=exp(-pow((f.x-0.10)/0.045,2.0));
    let value=clamp(0.12+f.y*0.88-dx*0.30-dy*0.40,0.0,1.0);
    var glass=mix(params.base,params.bright,value);
    glass=mix(glass,params.accent,clamp(edge*0.44+rim*0.35+(gleam+echo)*0.95,0.0,1.0));
    glass=mix(glass,params.base,fracture*0.48);
    let dispersion=vec3f(red, min(a,original.a),blue)*0.25;
    let expanded=max(coverage(p+vec2f(0.8,0.6)*s),coverage(p-vec2f(0.8,0.6)*s));
    let bevel=expanded*(1.0-original.a)*min(0.65*sqrt(params.intensity),0.95);
    let glint=expanded*(1.0-original.a)*gleam*0.7;
    let alpha=clamp(original.a*glass.a+(bevel+glint)*params.accent.a,0.0,1.0);
    let rgb=clamp(glass.rgb+dispersion,vec3f(0),vec3f(1));
    let colour=rgb*original.a*glass.a+mix(params.base.rgb,params.accent.rgb,clamp(gleam+edge,0.0,1.0))*(bevel+glint)*params.accent.a;
    return mix(original,contrast(vec4f(min(colour,vec3f(alpha)),alpha)),strength);
}

// A curling, shallow-volume fire front crosses the pane and scorches captured ink.
struct Parameters { intensity:f32,speed:f32,
    base:vec4f,bright:vec4f,accent:vec4f }
@group(1) @binding(0) var<uniform> params:Parameters;
fn dp()->f32 {
    if text.duration<=0.0{return fract(text.time*params.speed/4.8);}
    let r=clamp(params.speed,0.25,4.0);
    return text.progress*r/(1.0+text.progress*(r-1.0));
}
fn calm(a:f32,b:f32,x:f32)->f32{
    let t=clamp((x-a)/(b-a),0.0,1.0);return t*t*t*(t*(t*6.0-15.0)+10.0);
}
fn turn(p:vec2f,center:vec2f,r:f32,spin:f32)->vec2f {
    let d=p-center;let a=spin*exp(-dot(d,d)/(r*r));
    return center+vec2f(d.x*cos(a)-d.y*sin(a),d.x*sin(a)+d.y*cos(a));
}
fn effect(p:vec2f)->vec4f {
    if params.intensity<=0.0 || params.speed<=0.0{return sampleText(p);}
    let t=dp();let s=text.effect_scale*sqrt(params.intensity);let strength=min(params.intensity,1.0)
        *max(params.base.a,max(params.bright.a,params.accent.a));
    let center=textBaseline()-text.text_size.y*0.35;
    let front=mix(text.paint_offset.x-180.0*s,text.paint_offset.x+text.surface.x+240.0*s,calm(0.02,0.76,t));
    let q=vec2f((p.x-front)/s,(p.y-center)/s);
    let life=calm(0.0,0.10,t)*(1.0-calm(0.68,0.86,t))*strength;
    var fire=vec4f(0);
    if q.x>-360.0 && q.x<90.0 && abs(q.y)<155.0 && life>0.001 {
        var v=turn(q,vec2f(-65,-40),58.0,3.5+t*2.0);
        v=turn(v,vec2f(-170,38),70.0,-3.0-t*1.4);
        let velocity=vec3f(t*8.0,t*1.7,text.seed*0.01);
        let n=noise3D(vec3f(v/48.0,0.0)-velocity,1.0);
        let curl=noise3D(vec3f(v/26.0+vec2f(n*1.9),3.0)-velocity,0.0);
        let shaped=v+vec2f(n*23.0,curl*24.0);
        let envelope=exp(-pow((shaped.x+115.0)/140.0,2.0)-pow(shaped.y/55.0,2.0));
        let lead=1.0-calm(-8.0,55.0,shaped.x);
        var density=0.0;var hot=0.0;
        for(var j=0;j<5;j++){
            let z=(f32(j)-2.0)*0.45;
            let coarse=noise3D(vec3f(shaped/22.0,z)-velocity,0.0);
            let fine=noise3D(vec3f(shaped/8.0+vec2f(coarse),z+9.0)-velocity*1.7,0.0);
            let detail=noise3D(vec3f(shaped/3.6+vec2f(fine),z+19.0)-velocity*2.3,0.0);
            let d=clamp(envelope*1.40+coarse*0.84+fine*0.40+detail*0.15-0.78-abs(z)*0.14,0.0,1.0)*lead;
            density+=d*0.42;
            hot+=d*d*0.7;
        }
        let alpha=(1.0-exp(-density*2.4))*life;
        let colour=mix(mix(params.base,params.bright,clamp(density*1.5,0.0,1.0)),params.accent,clamp(hot*0.65,0.0,1.0));
        fire=vec4f(colour.rgb*alpha*colour.a,alpha*colour.a);
    }
    // Captured ink and its protected contour are zero beyond this rectangle.
    // Empty pane pixels need no glyph/noise sampling after the fire-front test.
    if any(p<vec2f(-4)) || any(p>text.text_size+vec2f(4)){return fire;}
    let original=sampleText(p);
    let behind=calm(-7.0*s,18.0*s,front-p.x);
    let scorch=behind*calm(0.01,0.16,t)*(1.0-calm(0.68,0.995,t))*strength;
    let heat=exp(-pow((p.x-front+30.0*s)/(48.0*s),2.0))*life;
    let grain=noise3D(vec3f(p/(2.8*s),text.seed*0.02),0.0)*0.5+0.5;
    let light=smoothstep(0.3,0.8,dot(text.background.rgb,vec3f(0.2126,0.7152,0.0722)));
    let charred=mix(vec3f(0.34,0.30,0.28),vec3f(0.20,0.17,0.16),light)*(0.65+grain*0.7);
    let charcoal=mix(charred,params.bright.rgb,heat*0.7+pow(grain,8.0)*scorch*0.28);
    let native=original.rgb/max(original.a,0.001);
    let inkColour=mix(native,charcoal,scorch);
    let ink=vec4f(inkColour*original.a,original.a);
    // A tiny background-colored contour keeps the scorched glyphs legible in the front.
    let border=max(max(coverage(p+vec2f(0.8,0)),coverage(p-vec2f(0.8,0))),
        max(coverage(p+vec2f(0,0.8)),coverage(p-vec2f(0,0.8))));
    fire=mix(fire,vec4f(text.background.rgb*fire.a,fire.a),border*0.8);
    return ink+fire*(1.0-ink.a);
}

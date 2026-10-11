// The voice leaves the inscription: filled strokes empty into a thin, breathless outline.
struct Parameters { intensity:f32,speed:f32,base:vec4f,bright:vec4f,accent:vec4f }
@group(1) @binding(0) var<uniform> params:Parameters;
fn phase()->f32{
    if text.duration<=0.0{return fract(text.time*params.speed/4.0);}
    let rate=clamp(params.speed,0.25,4.0);
    return text.progress*rate/(1.0+text.progress*(rate-1.0));
}
fn effect(p:vec2f)->vec4f{
    let ink=sampleText(p);
    if params.intensity<=0.0 || params.speed<=0.0{return ink;}
    let t=phase();let x=p.x/max(text.text_size.x,1.0);
    let empty=smoothstep(0.025+x*0.20,0.24+x*0.20,t)
        *(1.0-smoothstep(0.72+x*0.10,0.90+x*0.08,t))*min(params.intensity,1.0);
    if empty<0.0001{return ink;}
    // Erode the captured ink, not its rectangular bounds. The remainder is its actual contour.
    let radius=0.52*clamp(text.effect_scale,0.5,2.0);
    var interior=ink.a;
    for(var i=0u;i<8u;i++){
        let a=f32(i)*0.785398;
        interior=min(interior,coverage(p+vec2f(cos(a),sin(a))*radius));
    }
    let contour=max(ink.a-interior,0.0);
    let outline=clamp(contour*1.45,0.0,ink.a);
    let a=mix(ink.a,outline*0.62,empty);
    let color=ink.rgb/max(ink.a,0.001);
    let light=smoothstep(0.3,0.8,dot(text.background.rgb,vec3f(0.2126,0.7152,0.0722)));
    let quiet=mix(params.bright.rgb,params.base.rgb,light);
    // A narrow remaining rim preserves the reading; the centre loses its substance.
    return vec4f(mix(color,quiet,empty*0.62)*a,a);
}

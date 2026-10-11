// Counters become watchful. The lettering itself blinks; nothing is drawn outside the glyphs.
const TEXT_FRAGMENTS:u32=1u;
struct Parameters { intensity:f32,speed:f32,base:vec4f,bright:vec4f,accent:vec4f }
@group(1) @binding(0) var<uniform> params:Parameters;
fn wh(n:f32)->f32{return fract(sin(n*127.1+text.seed)*43758.5453);}
fn wp()->f32{
    if text.duration<=0.0{return fract(text.time*params.speed/4.4);}
    let rate=clamp(params.speed,0.25,4.0);
    // Bias the action within a finite duration while keeping both endpoints exact.
    return text.progress*rate/(1.0+text.progress*(rate-1.0));
}
fn waking()->f32{
    if params.intensity<=0.0 || params.speed<=0.0{return 0.0;}
    return min(params.intensity,1.0)*smoothstep(0.05,0.26,wp())*(1.0-smoothstep(0.74,0.98,wp()));
}
fn counter(index:u32)->vec3f{
    let ink=glyphAt(index).ink;
    let radius=max(min(ink.z*0.24,ink.w*0.24),0.9);
    var best=ink.xy+ink.zw*0.5;var score=0.0;
    for(var y=0u;y<3u;y++){for(var x=0u;x<3u;x++){
        let p=ink.xy+ink.zw*(vec2f(f32(x),f32(y))+vec2f(1.0))/4.0;
        let rim=coverage(p+vec2f(radius,0))+coverage(p-vec2f(radius,0))
            +coverage(p+vec2f(0,radius))+coverage(p-vec2f(0,radius));
        let candidate=(1.0-coverage(p))*rim;
        if candidate>score{score=candidate;best=p;}
    }}
    return vec3f(best,score);
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
fn bounds(index:u32)->vec4f{return column(index);}
fn effect(p:vec2f)->vec4f{
    if waking()<0.0001{return sampleText(p);}
    return vec4f(0);
}
fn textVertex(index:u32,corner:vec2f)->SmudgyFragment{
    if waking()<0.0001 || (glyphsTruncated() && index>0u){return SmudgyFragment(vec2f(0),vec2f(0),1,0);}
    let box=bounds(index);let p=box.xy+corner*box.zw;
    return SmudgyFragment(p,p,1.0,1.0);
}
fn textFragment(original:vec4f,p:vec2f,index:u32)->vec4f{
    let seed=wh(f32(index)+5.0);let ink=glyphAt(index).ink;
    if any(p<ink.xy) || any(p>ink.xy+ink.zw){return original;}
    let eye=counter(index);
    if eye.z<2.1{return original;}
    let phase=wp();let wake=waking();
    let radius=vec2f(max(ink.z*0.36,1.8),max(min(ink.z*0.30,ink.w*0.28),1.5))
        *clamp(text.effect_scale,0.25,2.0);
    let q=(p-eye.xy)/radius;
    let blink=1.0-exp(-pow((phase-(0.42+seed*0.18))*45.0,2.0))*0.83*wake;
    let region=exp(-dot(q,q)*0.40)*wake;
    let opened=mix(1.0,0.66,wake);
    let at=eye.xy+(p-eye.xy)*vec2f(1.0,mix(1.0,opened/blink,region));
    let native=sampleText(at);
    let lid=(1.0-smoothstep(0.65,1.15,length(q*vec2f(1,1.0/blink))))
        *(1.0-native.a)*wake;
    let separate=sin(phase*8.0+seed*6.283185)*0.55;
    let gaze=mix(separate,-0.25,smoothstep(0.57,0.72,phase));
    let pupil=1.0-smoothstep(0.19,0.43,length(q-vec2f(gaze,0.05)));
    let edge=sampleText(eye.xy+vec2f(radius.x,0))+sampleText(eye.xy-vec2f(radius.x,0))
        +sampleText(eye.xy+vec2f(0,radius.y))+sampleText(eye.xy-vec2f(0,radius.y));
    let styleColor=edge.rgb/max(edge.a,0.001);
    let sclera=mix(styleColor,params.accent.rgb,0.42)*0.88;
    let iris=mix(sclera,text.background.rgb,pupil);
    let alpha=lid*0.96;
    return native+vec4f(iris*alpha,alpha)*(1.0-native.a);
}

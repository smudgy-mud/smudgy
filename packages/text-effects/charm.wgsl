// Paired letters reach toward each other. Their warmth and small movements become mutual.
const TEXT_FRAGMENTS:u32=1u;
struct Parameters { intensity:f32,speed:f32,base:vec4f,bright:vec4f,accent:vec4f }
@group(1) @binding(0) var<uniform> params:Parameters;
fn phase()->f32 {
    if text.duration<=0.0 { return fract(text.time*params.speed/4.6); }
    let rate=clamp(params.speed,0.25,4.0);
    return text.progress*rate/(1.0+text.progress*(rate-1.0));
}
fn power()->f32 {
    if params.intensity<=0.0 || params.speed<=0.0 { return 0.0; }
    return min(params.intensity,1.0)*smoothstep(0.0,0.18,phase())*(1.0-smoothstep(0.78,0.995,phase()));
}
fn partner(index:u32)->u32 {
    let other=index^1u;
    if other>=glyphCount() || glyphsTruncated() { return index; }
    let a=glyphAt(index);let b=glyphAt(other);
    if a.cluster.z!=b.cluster.z || any(b.ink.zw<=vec2f(0)) { return index; }
    return other;
}
fn column(index:u32)->vec4f {
    let g=glyphAt(index);var lo=g.advance.xy-vec2f(0,2);var hi=g.advance.xy+g.advance.zw+vec2f(0,2);
    let first=index==0u || glyphAt(index-1u).cluster.z!=g.cluster.z;
    let last=index+1u==glyphCount() || glyphAt(index+1u).cluster.z!=g.cluster.z;
    if !last { hi.x=glyphAt(index+1u).advance.x; }
    if first { lo.x=min(lo.x,g.ink.x)-2.0; }
    if last { hi.x=max(hi.x,g.ink.x+g.ink.z)+2.0; }
    if glyphsTruncated() { lo=vec2f(-2);hi=text.text_size+vec2f(2); }
    return vec4f(lo,hi-lo);
}
fn closeness(index:u32)->f32 {
    let x=glyphAt(index).advance.x/max(text.text_size.x,1.0);
    let arrive=smoothstep(0.03+x*0.16,0.31+x*0.16,phase());
    let beat=0.90+0.10*cos((phase()-0.44)*14.0);
    return arrive*beat*power();
}
fn effect(p:vec2f)->vec4f { if power()<0.0001 { return sampleText(p); } return vec4f(0); }
fn textVertex(index:u32,corner:vec2f)->SmudgyFragment {
    if power()<0.0001 || (glyphsTruncated() && index>0u) { return SmudgyFragment(vec2f(0),vec2f(0),1,0); }
    let box=column(index);let source=box.xy+corner*box.zw;
    let other=partner(index);let side=select(-1.0,1.0,other>index)*select(1.0,0.0,other==index);
    let pivot=vec2f(box.x+box.z*0.5,glyphBaseline(index));let local=source-pivot;
    let near=closeness(index)*min(text.effect_scale,2.0)*sqrt(params.intensity);
    let lean=side*near*0.085;
    let rotated=vec2f(local.x*cos(lean)-local.y*sin(lean),local.x*sin(lean)+local.y*cos(lean));
    let position=pivot+rotated+vec2f(side*near*0.65,0);
    return SmudgyFragment(position,source,1,1);
}
fn textFragment(original:vec4f,p:vec2f,index:u32)->vec4f {
    let box=column(index);let g=glyphAt(index);let near=closeness(index);
    let other=partner(index);let side=select(-1.0,1.0,other>index)*select(1.0,0.0,other==index);
    let y=clamp((glyphBaseline(index)-p.y)/max(g.ink.w,4.0),0.0,1.0);
    // The strokes themselves bow toward their neighbor, most at their shoulders.
    let reach=sin(y*3.141593)*side*near*0.65*text.effect_scale;
    let source=p-vec2f(reach,0);
    if source.x<box.x || source.x>box.x+box.z { return vec4f(0); }
    let ink=sampleText(source);
    let meeting=select(g.ink.x,g.ink.x+g.ink.z,side>0.0);
    let warm=exp(-pow((p.x-meeting)/max(g.ink.z*0.7,2.0),2.0));
    let light=0.50+0.30*warm+0.10*sin(y*3.141593);
    let rose=mix(params.base.rgb,params.bright.rgb,light);
    let native=ink.rgb/max(ink.a,0.001);
    let colour=mix(native,rose,near*0.75);
    let affection=exp(-pow((phase()-0.52)/0.13,2.0))*warm*near;
    return vec4f(mix(colour,params.accent.rgb,affection*0.55)*ink.a,ink.a);
}

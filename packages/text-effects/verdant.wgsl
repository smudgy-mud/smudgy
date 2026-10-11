// Captured lettering unrolls from living coils; all growth is made from source ink.
const TEXT_FRAGMENTS:u32=16u;
struct Parameters { intensity:f32,speed:f32,base:vec4f,bright:vec4f,accent:vec4f }
@group(1) @binding(0) var<uniform> params:Parameters;
fn phase()->f32 {
    if text.duration<=0.0 { return fract(text.time*params.speed/4.2); }
    let rate=clamp(params.speed,0.25,4.0);
    return text.progress*rate/(1.0+text.progress*(rate-1.0));
}
fn power()->f32 {
    if params.intensity<=0.0 || params.speed<=0.0 { return 0.0; }
    return min(params.intensity,1.0)*smoothstep(0.0,0.08,phase())*(1.0-smoothstep(0.86,0.995,phase()));
}
fn column(index:u32)->vec4f {
    let g=glyphAt(index);
    var lo=g.advance.xy-vec2f(0,2); var hi=g.advance.xy+g.advance.zw+vec2f(0,2);
    let first=index==0u || glyphAt(index-1u).cluster.z!=g.cluster.z;
    let last=index+1u==glyphCount() || glyphAt(index+1u).cluster.z!=g.cluster.z;
    if !last { hi.x=glyphAt(index+1u).advance.x; }
    if first { lo.x=min(lo.x,g.ink.x)-2.0; }
    if last { hi.x=max(hi.x,g.ink.x+g.ink.z)+2.0; }
    if glyphsTruncated() { lo=vec2f(-2);hi=text.text_size+vec2f(2); }
    return vec4f(lo,hi-lo);
}
fn growth(index:u32)->f32 {
    let x=glyphAt(index).advance.x/max(text.text_size.x,1.0);
    return smoothstep(0.025+x*0.24,0.35+x*0.24,phase());
}
fn effect(p:vec2f)->vec4f { return sampleText(p)*(1.0-power()); }
fn textVertex(instance:u32,corner:vec2f)->SmudgyFragment {
    let index=instance/16u;
    if power()<0.0001 || (glyphsTruncated() && index>0u) { return SmudgyFragment(vec2f(0),vec2f(0),1,0); }
    let box=column(index);let band=f32(instance%16u);
    let source=box.xy+vec2f(corner.x,(band+corner.y)/16.0)*box.zw;
    let baseline=glyphBaseline(index);let height=max(baseline-box.y,1.0);
    let grown=growth(index);let curl=(1.0-grown)*4.8*power()*sqrt(params.intensity);
    let h=baseline-source.y;let angle=curl*h/height;
    let side=select(-1.0,1.0,fract(sin(f32(index)*127.1+text.seed)*43758.5453)>0.5);
    let radius=height/max(curl,0.001);
    let curled=vec2f(side*(1.0-cos(angle))*radius,-sin(angle)*radius);
    let flat=vec2f(0,-h);
    let bend=mix(flat,curled,smoothstep(0.0,0.02,curl));
    let center=box.x+box.z*0.5;
    let width=1.0-(1.0-grown)*0.72*power();
    let reach=min(text.effect_scale,2.5);
    let position=vec2f(center+(source.x-center)*width+bend.x*reach,baseline+bend.y);
    return SmudgyFragment(position,source,1,power());
}
fn textFragment(ink:vec4f,p:vec2f,instance:u32)->vec4f {
    let index=instance/16u;let box=column(index);let grown=growth(index);
    let height=max(glyphBaseline(index)-box.y,1.0);
    let y=clamp((glyphBaseline(index)-p.y)/height,0.0,1.0);
    let q=p/max(text.effect_scale,0.25);
    let tissue=noise3D(vec3f(q*vec2f(0.14,0.07),text.seed*0.02),0.0);
    let dx=coverage(p+vec2f(0.6,0))-coverage(p-vec2f(0.6,0));
    let dy=coverage(p+vec2f(0,0.6))-coverage(p-vec2f(0,0.6));
    let relief=clamp(-dx*0.3-dy*0.4,-0.4,0.4);
    let veins=pow(0.5+0.5*sin(q.y*1.9+abs(q.x-box.x-box.z*0.5)*1.5+tissue),10.0);
    let tip=exp(-pow((y-grown)/0.20,2.0))*(1.0-grown);
    var leaf=mix(params.base.rgb,params.bright.rgb,clamp(0.68+tissue*0.25+relief,0.0,1.0));
    leaf=mix(leaf,params.accent.rgb,clamp(tip*0.65+veins*0.14,0.0,0.85));
    let native=ink.rgb/max(ink.a,0.001);
    let radius=0.35*(1.0-grown)*power();
    let body=max(ink.a,max(coverage(p+vec2f(radius,0)),coverage(p-vec2f(radius,0))));
    return vec4f(mix(native,leaf,power())*body,body);
}

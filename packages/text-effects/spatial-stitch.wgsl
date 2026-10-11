// A travelling stitch closes the torn banks of the lettering, leaving repaired source ink.
const TEXT_FRAGMENTS:u32=3u;
struct Parameters { intensity:f32,speed:f32,base:vec4f,bright:vec4f,accent:vec4f }
@group(1) @binding(0) var<uniform> params:Parameters;
fn phase()->f32 {
    if text.duration<=0.0 { return fract(text.time*params.speed/4.2); }
    let rate=clamp(params.speed,0.25,4.0);
    return text.progress*rate/(1.0+text.progress*(rate-1.0));
}
fn power()->f32 {
    if params.intensity<=0.0 || params.speed<=0.0 { return 0.0; }
    return min(params.intensity,1.0)*smoothstep(0.0,0.12,phase())*(1.0-smoothstep(0.88,0.995,phase()));
}
fn front()->f32 { return mix(-0.04,1.04,smoothstep(0.20,0.82,phase())); }
fn closed(x:f32)->f32 { return smoothstep(x-0.035,x+0.035,front()); }
fn seam(x:f32,index:u32)->f32 {
    let g=glyphAt(index);
    let wave=noise3D(vec3f(x/max(text.effect_scale,0.25)*0.12,3.0,text.seed*0.01),0.0);
    return glyphBaseline(index)-max(g.advance.w,12.0)*0.32+wave*1.0*text.effect_scale;
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
fn effect(p:vec2f)->vec4f { if power()<0.0001 { return sampleText(p); } return vec4f(0); }
fn textVertex(instance:u32,corner:vec2f)->SmudgyFragment {
    let index=instance/3u;let layer=instance%3u;
    if power()<0.0001 || (glyphsTruncated() && index>0u) { return SmudgyFragment(vec2f(0),vec2f(0),1,0); }
    let box=column(index);let source=box.xy+corner*box.zw;
    if layer==2u { return SmudgyFragment(source,source,1,1); }
    let x=(box.x+box.z*0.5)/max(text.text_size.x,1.0);
    let open=power()*(1.0-closed(x))*min(text.effect_scale,2.5)*sqrt(params.intensity);
    let baseline=glyphBaseline(index);let height=max(baseline-box.y,1.0);
    let foot=clamp((baseline-source.y)/height,0.0,1.0);
    let side=select(-1.0,1.0,layer==1u);
    let position=source+vec2f(side*open*0.6,side*open*3.0*select(1.0,foot,layer==1u));
    return SmudgyFragment(position,source,1,1);
}
fn textFragment(ink:vec4f,p:vec2f,instance:u32)->vec4f {
    let index=instance/3u;let layer=instance%3u;let s=text.effect_scale;
    let x=p.x/max(text.text_size.x,1.0);let y=seam(p.x,index);
    let gap=abs(p.y-y);
    let joined=closed(x);let open=power()*(1.0-joined);
    if layer<2u {
        if (p.y<y)!=(layer==0u) { return vec4f(0); }
        let torn=(1.0-smoothstep(0.0,0.85*s,gap))*open*0.65;
        let a=ink.a*(1.0-torn);
        let repair=exp(-pow((front()-x)/0.045,2.0))*power();
        let native=mix(ink.rgb/max(ink.a,0.001),params.base.rgb,torn*0.4);
        return vec4f(mix(native,params.bright.rgb,repair*0.65)*a,a);
    }
    if glyphsTruncated() { return vec4f(0); }
    let trail=exp(-max(0.0,front()-x)*22.0)*joined*(1.0-smoothstep(0.80,0.97,phase()));
    let stitch=sin(p.x/max(3.2*s,0.25)*3.141593);
    let path=y+stitch*2.5*s;
    let thread=1.0-smoothstep(0.25*s,0.65*s,abs(p.y-path));
    let nearby=max(coverage(p+vec2f(0,2.8*s)),coverage(p-vec2f(0,2.8*s)));
    let threadAlpha=thread*trail*nearby*power()*0.8;
    let needle=exp(-pow((x-front())*text.text_size.x/(2.0*s),2.0)-pow((p.y-y)/(1.4*s),2.0))*power();
    let a=clamp(threadAlpha+needle,0.0,1.0);
    let colour=mix(params.bright.rgb,params.accent.rgb,needle);
    return vec4f(colour*a,a);
}

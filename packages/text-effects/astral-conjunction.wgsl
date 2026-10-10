// Three projected inscriptions turn into conjunction. Their aligned source text becomes the hero.
const TEXT_FRAGMENTS:u32=3u;
struct Parameters { intensity:f32,speed:f32,base:vec4f,bright:vec4f,accent:vec4f }
@group(1) @binding(0) var<uniform> params:Parameters;
fn phase()->f32 {
    if text.duration<=0.0 { return fract(text.time*params.speed/5.2); }
    let rate=clamp(params.speed,0.25,4.0);
    return text.progress*rate/(1.0+text.progress*(rate-1.0));
}
fn power()->f32 {
    if params.intensity<=0.0 || params.speed<=0.0 { return 0.0; }
    return min(params.intensity,1.0)*smoothstep(0.0,0.15,phase())*(1.0-smoothstep(0.80,0.995,phase()));
}
fn orbit()->f32 { return smoothstep(0.0,0.22,phase())*(1.0-smoothstep(0.28,0.66,phase()))*power(); }
fn zoom()->f32 {
    let growth=1.5*smoothstep(0.16,0.60,phase())*power()*text.effect_scale*sqrt(params.intensity);
    let capacity=max((text.surface.x-24.0)/max(text.text_size.x+4.0,1.0)-1.0,0.0);
    return 1.0+capacity*(1.0-exp(-growth/max(capacity,0.001)));
}
fn pivot()->vec2f {
    let width=(text.text_size.x+4.0)*zoom()*0.5;
    let center=text.text_size.x*0.5;
    let low=text.paint_offset.x+width+8.0;
    let high=max(low,text.paint_offset.x+text.surface.x-width-8.0);
    let destination=mix(center,clamp(center,low,high),smoothstep(0.0,0.5,zoom()-1.0));
    return vec2f(destination,textBaseline());
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
fn effect(p:vec2f)->vec4f {
    if power()<0.0001 { return sampleText(p); }
    let q=p-pivot()+vec2f(0,text.text_size.y*0.38*zoom());
    let h=max(text.text_size.y,16.0);let extent=max(text.text_size.x*0.62,h*2.2)*zoom();
    var light=0.0;let o=orbit();
    // The ellipses and their stars have a small shared footprint. Skip empty pane pixels,
    // and omit the orbit work entirely once the inscriptions have aligned.
    let outer=h*1.7*zoom();
    let edge=1.0+0.85/(h*zoom());
    let footprint=(vec2f(extent,extent*abs(sin((1.0+phase()*0.28)*o)))+vec2f(outer))*edge+vec2f(8);
    if o>0.0001 && all(abs(q)<footprint) {
        for(var i=0u;i<3u;i++) {
            let side=select(-1.0,1.0,i%2u==0u);
            let a=side*(0.32+f32(i)*0.34+phase()*0.28)*o;
            let rotated=vec2f(q.x*cos(a)-q.y*sin(a),q.x*sin(a)+q.y*cos(a));
            let radius=vec2f(extent,h*(1.0+f32(i)*0.35)*zoom());
            let uv=rotated/radius;let angle=atan2(uv.y,uv.x);
            let d=abs(length(uv)-1.0)*min(radius.x,radius.y);
            let arc=(1.0-smoothstep(0.2,0.85,d))*pow(max(cos(angle*3.0+f32(i)*2.1+phase()*2.0),0.0),4.0);
            light+=arc*o*0.20;
            let starAt=vec2f(cos(phase()*2.0*side+f32(i)*2.1),sin(phase()*2.0*side+f32(i)*2.1))*radius;
            let starQ=rotated-starAt;
            light+=exp(-dot(starQ,starQ)/3.0)*o*0.65;
        }
    }
    let conjunction=exp(-pow((phase()-0.63)/0.055,2.0))*power();
    let crossLight=exp(-abs(q.x)/(1.3*text.effect_scale))*exp(-abs(q.y)/(h*1.8))
        +exp(-abs(q.y)/(0.9*text.effect_scale))*exp(-abs(q.x)/(h*3.0));
    light+=crossLight*conjunction*0.65;
    let a=clamp(light,0.0,0.85);
    let lightBackground=smoothstep(0.3,0.8,dot(text.background.rgb,vec3f(0.2126,0.7152,0.0722)));
    let tint=mix(mix(params.bright.rgb,params.accent.rgb,conjunction),params.base.rgb*0.48,lightBackground);
    return vec4f(tint*a,a);
}
fn textVertex(instance:u32,corner:vec2f)->SmudgyFragment {
    let index=instance/3u;let plane=instance%3u;
    if power()<0.0001 || (glyphsTruncated() && index>0u) { return SmudgyFragment(vec2f(0),vec2f(0),1,0); }
    let box=column(index);let source=box.xy+corner*box.zw;
    let word=vec2f(text.text_size.x*0.5,textBaseline());let local=source-word;
    let side=select(-1.0,1.0,plane==1u);let o=orbit();
    let yaw=select(0.12,side*(0.78+phase()*0.30),plane>0u)*o;
    let roll=select(0.0,side*0.24,plane>0u)*o;
    let arc=sin(local.x/max(text.text_size.x,1.0)*3.141593);
    let depth=-local.x*sin(yaw)+side*arc*text.text_size.y*o*0.75;
    let y=local.y+select(0.0,side*text.text_size.y*1.5*o,plane>0u)+side*arc*text.text_size.y*o*0.55;
    let x=local.x*cos(yaw);
    let rotated=vec2f(x*cos(roll)-y*sin(roll),x*sin(roll)+y*cos(roll));
    let camera=max(text.text_size.x*1.5,240.0);let w=max(0.65,1.0+depth/camera);
    let position=pivot()+rotated*zoom()/w;
    let alpha=select(1.0,o*0.34,plane>0u);
    return SmudgyFragment(position,source,w,alpha);
}
fn textFragment(ink:vec4f,p:vec2f,instance:u32)->vec4f {
    let plane=instance%3u;
    let lightBackground=smoothstep(0.3,0.8,dot(text.background.rgb,vec3f(0.2126,0.7152,0.0722)));
    let tint=select(params.bright.rgb,params.base.rgb,plane==2u)*(1.0-lightBackground*0.50);
    let echo=select(power()*0.25,0.82,plane>0u);
    return vec4f(mix(ink.rgb,tint*ink.a,echo),ink.a);
}

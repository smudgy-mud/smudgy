// Source ink burns to ash, opens into two curling plumes, and returns incandescent to its own letters.
// Thirty-two source pieces and thirty-two faint trails per glyph; no particle simulation or uploads.
const TEXT_FRAGMENTS:u32=64u;
struct Parameters { intensity:f32,speed:f32,base:vec4f,bright:vec4f,accent:vec4f }
@group(1) @binding(0) var<uniform> params:Parameters;
fn hash(n:f32)->f32 { return fract(sin(n*127.1+text.seed)*43758.5453); }
fn phase()->f32 {
    if text.duration<=0.0 { return fract(text.time*params.speed/5.6); }
    let rate=clamp(params.speed,0.25,4.0);
    return text.progress*rate/(1.0+text.progress*(rate-1.0));
}
fn power()->f32 {
    if params.intensity<=0.0 || params.speed<=0.0 { return 0.0; }
    return min(params.intensity,1.0)*smoothstep(0.0,0.10,phase())*(1.0-smoothstep(0.90,0.995,phase()));
}
fn calm(a:f32,b:f32,x:f32)->f32 {
    let t=clamp((x-a)/(b-a),0.0,1.0);
    return t*t*t*(t*(t*6.0-15.0)+10.0);
}
fn extent()->f32 { return min(max(text.text_size.y,16.0)*7.0*text.effect_scale,text.surface.x*0.43); }
fn pivot()->vec2f {
    let center=text.text_size.x*0.5;
    let low=text.paint_offset.x+extent()+8.0;
    let high=max(low,text.paint_offset.x+text.surface.x-extent()-8.0);
    return vec2f(clamp(center,low,high),textBaseline()-text.text_size.y*0.25);
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
fn plumeY(u:f32)->f32 {
    return -max(text.text_size.y,16.0)*(0.20+u*2.1+sin(u*3.141593)*1.45)*min(text.effect_scale,2.0);
}
fn plumePath(u:f32,t:f32)->vec2f {
    let h=max(text.text_size.y,16.0)*min(text.effect_scale,2.0);
    if u<=0.55 { return vec2f(u*extent(),plumeY(u)); }
    let curl=(u-0.55)/0.45;
    let angle=curl*3.8+(t-0.42)*1.2*smoothstep(0.0,0.35,curl);
    let radius=h*(0.70+curl*0.50);
    return vec2f(extent()*0.55+sin(angle)*radius,plumeY(0.55)+h*0.70-cos(angle)*radius);
}
fn effect(p:vec2f)->vec4f {
    let energy=power();if energy<0.0001 { return sampleText(p); }
    let t=phase();let q=p-pivot();let h=max(text.text_size.y,16.0);
    let flight=calm(0.10,0.30,t)*(1.0-calm(0.62,0.86,t))*energy;
    var glow=0.0;
    if abs(q.x)<extent()+h*1.5 && q.y<0.5*h*text.effect_scale
        && q.y>-5.0*h*text.effect_scale && flight>0.0001 {
        let mirrored=vec2f(abs(q.x),q.y);
        var distance=1e6;
        var previous=plumePath(0.0,t);
        for(var i=1u;i<=16u;i++) {
            let next=plumePath(f32(i)/16.0,t);
            let delta=next-previous;
            let along=clamp(dot(mirrored-previous,delta)/max(dot(delta,delta),0.001),0.0,1.0);
            distance=min(distance,length(mirrored-previous-delta*along));
            previous=next;
        }
        let width=h*0.34*text.effect_scale;
        let noise=noise3D(vec3f(q/(h*0.24),t*2.2+text.seed*0.01),0.0);
        let fine=noise3D(vec3f(q/(h*0.09)+vec2f(noise),t*3.0+17.0),0.0);
        let density=exp(-pow((distance+noise*width*0.7)/max(width,1.0),2.0));
        let ribbons=clamp(0.50+noise*0.40+fine*0.25,0.0,1.0);
        glow=density*ribbons*flight*0.66;
    }
    let restored=calm(0.73,0.88,t)*(1.0-calm(0.88,0.99,t))*energy;
    let halo=sampleTextLod(p,2.0).a*restored*0.32;
    let a=clamp(glow+halo,0.0,0.65);
    let heat=calm(0.32,0.62,t);
    let colour=mix(params.base.rgb,params.bright.rgb,0.4+heat*0.6);
    return vec4f(colour*a,a);
}
fn textVertex(instance:u32,corner:vec2f)->SmudgyFragment {
    let index=instance/64u;let piece=instance%32u;let trail=instance%64u>=32u;
    if power()<0.0001 || (glyphsTruncated() && index>0u) { return SmudgyFragment(vec2f(0),vec2f(0),1,0); }
    let box=column(index);let tile=vec2f(f32(piece%4u),f32(piece/4u));
    let size=box.zw/vec2f(4,8);let source=box.xy+(tile+corner)*size;
    let center=box.xy+(tile+vec2f(0.5))*size;
    let n=f32(index*32u+piece)+19.0;let r=hash(n);
    let t=max(phase()-select(0.0,0.022,trail),0.0);
    let depart=calm(0.06+r*0.10,0.34+r*0.05,t);
    let arrive=calm(0.53+r*0.07,0.88,t);
    let flight=depart*(1.0-arrive)*power();
    let side=select(-1.0,1.0,hash(n+3.0)>0.5);
    let u=0.05+hash(n+7.0)*0.90;
    let h=max(text.text_size.y,16.0);let s=text.effect_scale;
    let curl=(t-0.42)*3.0+hash(n+11.0)*6.283185;
    let spread=sin(u*3.141593)*h*0.32*s;
    let path=plumePath(u,t);
    let plume=pivot()+vec2f(side*path.x+cos(curl)*spread,
        path.y+sin(curl)*spread*(0.45+hash(n+2.0)*0.8));
    let lift=sin(flight*3.141593)*h*(0.4+hash(n+5.0))*s;
    let destination=mix(center,plume,flight)+vec2f(0,-lift);
    let local=source-center;
    let tangent=plumePath(min(u+0.01,1.0),t)-plumePath(max(u-0.01,0.0),t);
    let spin=(atan2(tangent.y,side*tangent.x)+(hash(n+13.0)-0.5)*1.2)*flight;
    let yaw=(hash(n+17.0)-0.5)*5.0*flight;
    let feather=local*vec2f(1.0+flight*1.4,1.0-flight*0.25);
    let rotated=vec2f(feather.x*cos(spin)-feather.y*sin(spin),feather.x*sin(spin)+feather.y*cos(spin));
    let depth=(hash(n+23.0)-0.5)*h*3.0*flight;
    let camera=max(h*9.0,160.0);let w=max(0.6,1.0+(depth+rotated.x*sin(yaw))/camera);
    let sizePulse=1.0+flight*(0.4+hash(n+29.0)*0.8)*sqrt(params.intensity);
    let position=destination+vec2f(rotated.x*cos(yaw),rotated.y)*sizePulse/w;
    let opacity=select(1.0,flight*0.25,trail);
    return SmudgyFragment(position,source,w,opacity);
}
fn textFragment(ink:vec4f,p:vec2f,instance:u32)->vec4f {
    let n=f32(instance/64u*32u+instance%32u)+19.0;
    let t=phase();let r=hash(n);let energy=power();
    let burnt=calm(0.02+r*0.06,0.14+r*0.08,t);
    let rekindle=calm(0.34+r*0.09,0.64,t);
    let cool=1.0-calm(0.87,0.995,t);
    let ash=mix(params.base.rgb*0.45,params.base.rgb,hash(n+3.0));
    let ember=mix(params.bright.rgb,params.accent.rgb,rekindle*(0.4+r*0.6));
    let material=mix(ash,ember,rekindle);
    let native=ink.rgb/max(ink.a,0.001);
    let colour=mix(native,material,burnt*cool*energy);
    return vec4f(colour*ink.a,ink.a);
}

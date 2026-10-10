// A staged inscription, or charges travelling along connections between shaped glyphs.
const TEXT_FRAGMENTS: u32 = 1u;
struct Parameters { intensity: f32, speed: f32, variant: f32,
    base: vec4f, bright: vec4f, accent: vec4f }
@group(1) @binding(0) var<uniform> params: Parameters;
fn rp() -> f32 { return select(fract(text.time*params.speed/3.6),text.progress,text.duration>0.0); }
fn visibleTint(c:vec4f)->vec4f {
    let light=smoothstep(0.30,0.75,dot(text.background.rgb,vec3f(0.2126,0.7152,0.0722)));
    return vec4f(mix(c.rgb,params.base.rgb,light*0.65),c.a);
}
fn power() -> f32 {
    if params.speed<=0.0 || params.intensity<=0.0 { return 0.0; }
    return min(params.intensity,2.0)*smoothstep(0.02,0.16,rp())*(1.0-smoothstep(0.74,0.98,rp()));
}
fn line(p: vec2f,a: vec2f,b: vec2f) -> f32 {
    let v=b-a;return length(p-a-v*clamp(dot(p-a,v)/max(dot(v,v),0.0001),0.0,1.0));
}
fn anchor(index:u32) -> vec2f {
    let glyph=glyphAt(index);
    return vec2f(glyph.advance.x+glyph.advance.z*0.5,glyphBaseline(index)-glyph.advance.w*0.30);
}
fn effect(p: vec2f) -> vec4f {
    let source=sampleText(p);let strength=power();
    if strength<0.0001 || params.variant>0.5 { return source; }
    let scale=text.effect_scale;let center=vec2f(text.text_size.x*0.5,textBaseline()-text.text_size.y*0.28);
    // Scale the clearance around reserved text, rather than multiplying its width.
    let radius=vec2f(max(text.text_size.x*0.5+12.0*scale,36.0*scale),(text.text_size.y*0.92+10.0)*scale);
    let q=(p-center)/radius;let angle=atan2(q.y,q.x);let theta=fract(angle/6.2831853+0.5);
    let inscription=smoothstep(0.04,0.50,rp());
    let front=smoothstep(theta-0.025,theta+0.025,inscription);
    let ring=exp(-pow((length(q)-1.0)*min(radius.x,radius.y)/(0.9*scale),2.0))*front;
    let inner=exp(-pow((length(q)-0.86)*min(radius.x,radius.y)/(0.5*scale),2.0))*front*0.32;
    let writing=exp(-pow((theta-inscription)*32.0,2.0))*exp(-pow((length(q)-1.0)*min(radius.x,radius.y)/3.0,2.0));
    var runes=0.0;
    for(var i=0;i<8;i++) {
        let a=f32(i)*0.785398+0.392699;
        let c=vec2f(cos(a),sin(a))*1.15;
        let uv=(q-c)*radius/scale;
        let glyph=line(uv,vec2f(-3,-4),vec2f(2,4));
        let bar=line(uv,vec2f(-3,1),vec2f(4,-2));
        let notch=line(uv,vec2f(2,4),vec2f(4,0));
        let reveal=smoothstep(0.22+f32(i)*0.028,0.29+f32(i)*0.028,rp());
        runes=max(runes,exp(-pow(min(glyph,min(bar,notch))/0.65,2.0))*reveal);
    }
    let lock=exp(-pow((rp()-0.57)*18.0,2.0));
    let aura=exp(-pow((length(q)-1.0)*min(radius.x,radius.y)/5.0,2.0))*0.17;
    let energy=clamp((ring+inner+runes+writing*0.7+aura+lock*ring)*strength,0.0,1.0);
    let tint=visibleTint(mix(params.bright,params.accent,clamp(writing+lock*0.8,0.0,1.0)));
    let a=energy*tint.a*(1.0-source.a);
    return source+vec4f(tint.rgb*a,a);
}
fn textVertex(index:u32,corner:vec2f) -> SmudgyFragment {
    if params.variant<0.5 || power()<0.0001 || index+1u>=glyphCount() || glyphsTruncated() {
        return SmudgyFragment(vec2f(0),vec2f(0),1.0,0.0);
    }
    let a=anchor(index);let b=anchor(index+1u);let h=text.text_size.y*text.effect_scale;
    let lo=min(a,b)-vec2f(5,h*0.72);let hi=max(a,b)+vec2f(5,h*0.72);
    let p=mix(lo,hi,corner);
    return SmudgyFragment(p,p,1.0,1.0);
}
fn textFragment(unused:vec4f,p:vec2f,index:u32) -> vec4f {
    let a=anchor(index);let b=anchor(index+1u);let s=text.effect_scale;
    let curve=select(-1.0,1.0,index%2u==0u)*text.text_size.y*1.10*s;
    let mid=(a+b)*0.5+vec2f(0,curve);
    var previous=a;var distance=1000.0;var along=0.0;
    for(var i=1;i<=10;i++) {
        let t=f32(i)/10.0;
        let q=(1.0-t)*(1.0-t)*a+2.0*(1.0-t)*t*mid+t*t*b;
        let d=line(p,previous,q);
        if d<distance { distance=d;along=t; }
        previous=q;
    }
    // Two distinct packets make one sweep. The path stays dim between their passages.
    let progress=rp()*2.6;
    let location=(f32(index)+along)/max(f32(glyphCount()-1u),1.0);
    let packet=exp(-pow((location-fract(progress))*12.0,2.0));
    let etched=smoothstep(location*0.23,location*0.23+0.08,rp());
    let core=exp(-pow(distance/max(0.65*s,0.35),2.0));
    let glow=exp(-pow(distance/max(3.5*s,0.7),2.0));
    let energy=(core*(0.45+packet*0.55)+glow*packet*0.32)*power()*etched;
    let inkMask=max(coverage(p),max(coverage(p+vec2f(0,0.7)),coverage(p-vec2f(0,0.7))));
    let tint=visibleTint(mix(params.base,mix(params.bright,params.accent,packet),0.4+packet*0.6));
    let alpha=clamp(energy,0.0,1.0)*tint.a*(1.0-inkMask);
    return vec4f(tint.rgb*alpha,alpha);
}

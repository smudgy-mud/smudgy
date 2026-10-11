// Living material belongs to captured ink. Fungal bodies grow from its strokes; wounded strokes heal.
const TEXT_FRAGMENTS:u32=2u;
struct Parameters { intensity:f32,speed:f32,variant:f32,
    base:vec4f,bright:vec4f,accent:vec4f }
@group(1) @binding(0) var<uniform> params:Parameters;
fn lp()->f32{
    if text.duration<=0.0{return fract(text.time*params.speed/4.5);}
    let rate=clamp(params.speed,0.25,4.0);
    // Bias the action within a finite duration while keeping both endpoints exact.
    return text.progress*rate/(1.0+text.progress*(rate-1.0));
}
fn life()->f32{
    if params.intensity<=0.0 || params.speed<=0.0{return 0.0;}
    return min(params.intensity,1.5)*smoothstep(0.02,0.15,lp())*(1.0-smoothstep(0.80,0.98,lp()));
}
fn field(p:vec2f)->f32{
    let q=p/(max(text.text_size.y,16.0)*text.effect_scale);
    let n=noise3D(vec3f(q*2.4,text.seed*0.023),0.0);
    return noise3D(vec3f(q*7.0+vec2f(n*0.9),text.seed*0.037+7.0),0.0);
}
fn normal(p:vec2f,step:f32)->vec3f{
    let gradient=vec2f(sampleTextLod(p+vec2f(step,0),1.0).a-sampleTextLod(p-vec2f(step,0),1.0).a,
        sampleTextLod(p+vec2f(0,step),1.0).a-sampleTextLod(p-vec2f(0,step),1.0).a);
    return normalize(vec3f(-gradient*3.0,0.65));
}
fn fungus(p:vec2f)->vec4f{
    let power=life();let s=text.effect_scale;
    let location=clamp(p.x/max(text.text_size.x,1.0),0.0,1.0);
    let grown=smoothstep(location*0.38,location*0.38+0.26,lp());
    let amount=min(power*grown,1.0);
    let flow=field(p);let q=p/s;
    let bend=vec2f(flow,noise3D(vec3f(q*0.055,text.seed*0.057+23.0),0.0));
    let at=p+bend*max(text.text_size.y,16.0)*0.13*amount*s;
    let native=sampleText(at);
    let micro=noise3D(vec3f(q*0.12,text.seed*0.031+11.0),0.0);
    let fine=noise3D(vec3f(q*0.40,text.seed*0.049+19.0),0.0);
    let radius=(1.6+flow*0.9)*s*amount;
    var hull=native.a;
    for(var i=0u;i<6u;i++){
        let angle=f32(i)*1.047198;
        hull=max(hull,coverage(at+vec2f(cos(angle),sin(angle))*radius));
    }
    let soft=sampleTextLod(at,1.0).a;
    let fuzz=smoothstep(0.045,0.19,soft)*smoothstep(-0.25,0.40,micro)*0.36*amount;
    let body=max(hull,fuzz);
    let n=normal(at,0.75*s);
    let diffuse=max(dot(n,normalize(vec3f(-0.5,-0.7,1.0))),0.0);
    let sheen=pow(max(dot(reflect(normalize(vec3f(0.4,0.7,-1.0)),n),vec3f(0,0,1)),0.0),12.0);
    let pores=1.0-smoothstep(0.10,0.23,abs(fine+flow*0.2));
    let lichen=mix(params.base.rgb,params.bright.rgb,smoothstep(-0.35,0.4,micro));
    let velvet=mix(lichen,params.accent.rgb,clamp(flow*0.35+0.12,0.0,0.4));
    let material=velvet*(0.48+diffuse*0.70)*(1.0-pores*0.08)+params.accent.rgb*sheen*0.12;
    let light=smoothstep(0.3,0.8,dot(text.background.rgb,vec3f(0.2126,0.7152,0.0722)));
    let rgb=mix(material,material*0.60,light);
    let a=mix(native.a,body,amount)*mix(1.0,params.bright.a,amount);
    let sourceColor=native.rgb/max(native.a,0.001);
    return vec4f(mix(sourceColor,rgb,amount)*a,a);
}
fn effect(p:vec2f)->vec4f{
    if life()<0.0001{return sampleText(p);}
    if any(p<vec2f(-12)*text.effect_scale) || any(p>text.text_size+vec2f(12)*text.effect_scale){
        return sampleText(p);
    }
    if params.variant>0.5{return vec4f(0);}
    return fungus(p);
}
fn wound(index:u32)->f32{
    if glyphsTruncated() || any(glyphAt(index).ink.zw<=vec2f(0)){return 0.0;}
    let location=f32(index)/max(f32(glyphCount()-1u),1.0);
    let front=smoothstep(0.30,0.84,lp());
    let closed=smoothstep(location-0.07,location+0.07,front);
    return smoothstep(0.02,0.22,lp())*(1.0-closed)*min(life(),1.0);
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
fn piece(instance:u32)->vec4f{
    let index=instance/2u;let glyph=glyphAt(index);let box=column(index);
    var lo=box.xy;var hi=box.xy+box.zw;
    let cut=select(clamp(glyph.ink.x+glyph.ink.z*0.52,lo.x,hi.x),
        text.text_size.x*0.5,glyphsTruncated());
    if instance%2u==0u{hi.x=cut;}else{lo.x=cut;}
    return vec4f(lo,hi-lo);
}
fn textVertex(instance:u32,corner:vec2f)->SmudgyFragment{
    if params.variant<0.5 || life()<0.0001 || (glyphsTruncated() && instance/2u>0u){
        return SmudgyFragment(vec2f(0),vec2f(0),1,0);
    }
    let index=instance/2u;let box=piece(instance);let source=box.xy+corner*box.zw;
    let hurt=wound(index);let side=select(-1.0,1.0,instance%2u==1u);
    let baseline=glyphBaseline(index);let pivot=vec2f(box.x+box.z*0.5,baseline);
    let angle=side*hurt*0.085*min(text.effect_scale,2.0);
    let local=source-pivot;
    let rotated=vec2f(local.x*cos(angle)-local.y*sin(angle),local.x*sin(angle)+local.y*cos(angle));
    let position=pivot+rotated+vec2f(side*hurt*2.2*text.effect_scale,0);
    return SmudgyFragment(position,source,1,1);
}
fn textFragment(original:vec4f,p:vec2f,instance:u32)->vec4f{
    if glyphsTruncated(){return original;}
    let hurt=wound(instance/2u);let box=piece(instance);let s=text.effect_scale;
    let cut=select(box.x+box.z,box.x,instance%2u==1u);
    let distance=abs(p.x-cut);
    let rough=noise3D(vec3f(p/s*vec2f(0.11,0.16),text.seed*0.023),0.0);
    let edge=(1.0-smoothstep(0.1,1.7+rough*0.8,distance/s))*hurt;
    let alpha=original.a*(1.0-edge*0.78);
    let inner=(1.0-smoothstep(0.0,3.0,distance/s))*hurt;
    let grain=noise3D(vec3f(p/s*vec2f(0.12,0.05),text.seed*0.039),0.0);
    let woody=mix(params.base.rgb*0.8,params.base.rgb*1.4,0.5+grain*0.3);
    let native=original.rgb/max(original.a,0.001);
    let color=mix(native,woody,inner*0.50);
    // Fresh growth crosses only the wounded strokes as their two banks reconnect.
    let location=f32(instance/2u)/max(f32(glyphCount()-1u),1.0);
    let front=smoothstep(0.30,0.84,lp());
    let joined=exp(-pow((front-location)/0.065,2.0))*smoothstep(0.25,0.40,lp())*life();
    let sap=mix(params.bright.rgb,params.accent.rgb,smoothstep(0.65,1.0,joined));
    return vec4f(mix(color,sap,joined*0.68)*alpha,alpha);
}

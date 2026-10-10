// Growing roots attach to sampled glyph ink, rather than a shared horizontal border.
const TEXT_FRAGMENTS: u32 = 1u;
struct Parameters { intensity: f32, speed: f32, base: vec4f, bright: vec4f, accent: vec4f }
@group(1) @binding(0) var<uniform> params: Parameters;
fn hash(n: f32) -> f32 { return fract(sin(n*127.1+text.seed)*43758.5453); }
fn growth() -> f32 {
    if params.intensity<=0.0 || params.speed<=0.0 { return 0.0; }
    let p = select(fract(text.time*params.speed/4.0),text.progress,text.duration>0.0);
    return smoothstep(0.02,0.85,p);
}
fn attachment(index: u32) -> vec2f {
    let ink = glyphAt(index).ink;
    var best = vec2f(ink.x+ink.z*0.5,ink.y+ink.w);
    var score = 0.0;
    for (var y=0u;y<4u;y++) { for (var x=0u;x<4u;x++) {
        let point = ink.xy+ink.zw*vec2f((f32(x)+0.5)/4.0,0.55+(f32(y)+0.5)*0.1125);
        let candidate = coverage(point)*(0.8+f32(y)*0.1);
        if candidate>score { score=candidate;best=point; }
    }}
    return best;
}
fn effect(p: vec2f) -> vec4f { return sampleText(p); }
fn textVertex(index: u32, corner: vec2f) -> SmudgyFragment {
    let glyph = glyphAt(index);
    if growth()<=0.0001 || glyph.ink.z<=0.0 || glyph.ink.w<=0.0 {
        return SmudgyFragment(vec2f(0),vec2f(0),1.0,0.0);
    }
    let s=text.effect_scale;
    let width = max(glyph.advance.z,8.0);
    let lo=vec2f(-width-10.0,-5.0)*s;
    let hi=vec2f(width+10.0,20.0)*s;
    let local=mix(lo,hi,corner);
    return SmudgyFragment(attachment(index)+local,local,1.0,min(params.intensity,1.0));
}
fn segment(p: vec2f, a: vec2f, b: vec2f) -> vec2f {
    let v=b-a;let t=clamp(dot(p-a,v)/max(dot(v,v),0.001),0.0,1.0);
    return vec2f(length(p-a-v*t),t);
}
fn trunk(t: f32, index: u32) -> vec2f {
    let seed=hash(f32(index)+3.0)*6.2831853;
    return vec2f((sin(t*3.4+seed)-sin(seed))*3.8,t*9.0);
}
fn textFragment(unused: vec4f, local: vec2f, index: u32) -> vec4f {
    let p=local/text.effect_scale;
    let grow=growth();let seed=hash(f32(index)+5.0);
    var body=0.0;var ridge=0.0;
    for (var i=0;i<8;i++) {
        let a=f32(i)/8.0;let b=min(f32(i+1)/8.0,grow);
        if b<=a { break; }
        let d=segment(p,trunk(a,index),trunk(b,index));
        let age=smoothstep(a,a+0.18,grow);
        let width=(0.78*(1.0-a)+0.22)*age;
        body=max(body,1.0-smoothstep(width,width+0.6,d.x));
        ridge=max(ridge,exp(-pow(d.x/max(width*0.4,0.25),2.0))*0.45);
    }
    // Branch reach follows the shaped advance, so proportional letters bind naturally.
    let reach=min(max(glyphAt(index).advance.z,7.0),34.0);
    for (var branch=0;branch<4;branch++) {
        let at=0.18+f32(branch)*0.16;
        let maturity=smoothstep(at+0.10,at+0.40,grow);
        let root=trunk(at,index);
        let side=select(-1.0,1.0,branch%2==0);
        let length=(reach*(0.45+seed*0.32)+3.0)*maturity;
        var previous=root;
        for (var i=1;i<=5;i++) {
            let t=f32(i)/5.0;
            let point=root+vec2f(side*length*t,
                ((3.5+seed*3.0)*t+t*t*2.0+sin(t*4.0+seed*2.0)*1.4*t)*maturity);
            let d=segment(p,previous,point);
            let width=(0.68*(1.0-t)+0.16)*maturity;
            body=max(body,(1.0-smoothstep(width,width+0.5,d.x))*step(0.001,maturity));
            previous=point;
        }
        let tip=previous;
        for (var fork=0;fork<2;fork++) {
            let f=select(-1.0,1.0,fork==1);
            let end=tip+vec2f(side*(2.0+seed*2.0),f*2.1+1.5)*maturity;
            body=max(body,(1.0-smoothstep(0.12,0.65,segment(p,tip,end).x))*maturity);
        }
    }
    let tint=mix(mix(params.base,params.bright,0.24+ridge),params.accent,ridge*0.5);
    let alpha=clamp(body,0.0,1.0)*tint.a;
    return vec4f(tint.rgb*alpha,alpha);
}

// A wide eldritch seal: anticipation, counter-rotation, locking, and retreat.
struct Parameters { intensity:f32,speed:f32,base:vec4f,bright:vec4f,accent:vec4f }
@group(1) @binding(0) var<uniform> params:Parameters;
fn eh(n:f32)->f32 {return fract(sin(n*127.1+text.seed)*43758.5453);}
fn segment(p:vec2f,a:vec2f,b:vec2f)->f32 {
    let v=b-a;return length(p-a-v*clamp(dot(p-a,v)/max(dot(v,v),0.001),0.0,1.0));
}
fn effect(p:vec2f)->vec4f {
    let ink=sampleText(p);
    if params.speed<=0.0 || params.intensity<=0.0 {return ink;}
    let phase=select(fract(text.time*params.speed/4.0),text.progress,text.duration>0.0);
    let envelope=smoothstep(0.01,0.22,phase)*(1.0-smoothstep(0.72,0.99,phase));
    if envelope<0.0001 {return ink;}
    let scale=text.effect_scale;let h=text.text_size.y;
    let center=vec2f(text.text_size.x*0.5,textBaseline()-h*0.28);
    let q=(p-center)/scale;let r=length(q);let angle=atan2(q.y,q.x);
    let size=max(text.text_size.x*0.33,h*2.0)+34.0;
    let turn=smoothstep(0.06,0.60,phase);
    let lock=exp(-pow((phase-0.53)*17.0,2.0));
    let drawing=smoothstep(0.14,0.42,phase);
    var sigil=0.0;var halo=0.0;
    for(var layer=0;layer<3;layer++) {
        let radius=size*(0.66+f32(layer)*0.30);
        let direction=select(-1.0,1.0,layer%2==0);
        let theta=angle+direction*turn*(0.36+f32(layer)*0.18);
        let sector=fract(theta/6.2831853*7.0+f32(layer)*0.27);
        let broken=smoothstep(0.03,0.12,sector)*(1.0-smoothstep(0.72,0.86,sector));
        let ring=exp(-pow((r-radius)/0.85,2.0))*broken*drawing;
        let outer=exp(-pow((r-radius)/4.0,2.0))*broken*drawing;
        sigil+=ring*(0.75+lock);halo+=outer*0.16;
    }
    // Seven asymmetric hooked rays settle into the seal rather than endlessly spin.
    for(var i=0;i<7;i++) {
        let theta=f32(i)*0.897598+turn*0.42;
        let axis=vec2f(cos(theta),sin(theta));let side=vec2f(-axis.y,axis.x);
        let a=axis*size*0.43;
        let b=axis*size*(0.83+eh(f32(i)+1.0)*0.20)+side*size*0.12;
        let c=b+side*size*0.12-axis*size*0.15;
        let stroke=min(segment(q,a,b),segment(q,b,c));
        let reveal=smoothstep(0.20+f32(i)*0.017,0.32+f32(i)*0.017,phase);
        sigil+=exp(-pow(stroke/0.85,2.0))*reveal;
        halo+=exp(-pow(stroke/4.0,2.0))*reveal*0.12;
    }
    let inward=1.0-smoothstep(size*0.9,size*2.6,r);
    let dark=clamp(inward*envelope*0.58*params.intensity,0.0,0.80);
    let field=mix(text.background.rgb,params.base.rgb,0.85);
    let gleam=clamp((sigil+halo)*envelope*params.intensity,0.0,1.0);
    let tint=mix(params.bright,params.accent,clamp(lock+sigil*0.25,0.0,1.0));
    let glowAlpha=gleam*tint.a;
    let under=vec4f(tint.rgb*glowAlpha+field*dark*(1.0-glowAlpha),glowAlpha+dark*(1.0-glowAlpha));
    // Keep the shaped inscription clear through the darkest frame and the locking flash.
    let stroke=max(max(coverage(p+vec2f(1,0)),coverage(p-vec2f(1,0))),
        max(coverage(p+vec2f(0,1)),coverage(p-vec2f(0,1))));
    let outline=text.background.rgb*stroke;
    let backdrop=vec4f(outline,stroke)+under*(1.0-stroke);
    return ink+backdrop*(1.0-ink.a);
}

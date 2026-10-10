// Pane-scale attention signals. Native styled text is always drawn over the signal.
struct Parameters { intensity:f32,speed:f32,variant:f32,base:vec4f,bright:vec4f,accent:vec4f }
@group(1) @binding(0) var<uniform> params:Parameters;
fn phase()->f32 {
    if text.duration<=0.0 { return fract(text.time*params.speed/2.4); }
    let rate=clamp(params.speed,0.25,4.0);
    return text.progress*rate/(1.0+text.progress*(rate-1.0));
}
fn ease(a:f32,b:f32,t:f32)->f32 {
    let u=clamp((t-a)/(b-a),0.0,1.0);
    return u*u*u*(u*(u*6.0-15.0)+10.0);
}
fn pulse(t:f32)->f32 { return ease(0.0,0.09,t)*(1.0-ease(0.66,1.0,t)); }
fn reach(center:vec2f)->f32 {
    let near=text.paint_offset-center;let far=near+text.surface;
    return max(max(length(near),length(far)),max(length(vec2f(near.x,far.y)),length(vec2f(far.x,near.y))));
}
fn box(p:vec2f,size:vec2f,r:f32)->f32 {
    let q=abs(p)-size+vec2f(r);
    return length(max(q,vec2f(0)))+min(max(q.x,q.y),0.0)-r;
}
fn segment(p:vec2f,a:vec2f,b:vec2f)->f32 {
    let v=b-a;return length(p-a-v*clamp(dot(p-a,v)/max(dot(v,v),0.001),0.0,1.0));
}
fn paint(body:f32,core:f32,haze:f32)->vec4f {
    let shade=mix(mix(params.base,params.bright,clamp(body,0.0,1.0)),params.accent,clamp(core,0.0,1.0));
    let a=clamp(body*0.70+core*0.95+haze,0.0,0.90)*min(params.intensity,1.0)*shade.a;
    let light=smoothstep(0.3,0.8,dot(text.background.rgb,vec3f(0.2126,0.7152,0.0722)));
    let colour=mix(shade.rgb,params.base.rgb*0.48,light*0.88);
    return vec4f(colour*a,a);
}
fn sonar(p:vec2f,t:f32)->vec4f {
    let center=text.text_size*0.5;let s=text.effect_scale*sqrt(params.intensity);
    let q=p-center;let r=length(q/vec2f(1.15,0.83));
    let far=reach(center)*1.35;var body=0.0;var core=0.0;var haze=0.0;
    for(var i=0u;i<3u;i++) {
        let age=clamp((t-f32(i)*0.14)/0.72,0.0,1.0);
        let radius=mix(max(text.text_size.y*0.7,14.0),far,ease(0.0,1.0,age));
        let d=r-radius;let energy=pulse(age)*(1.0-f32(i)*0.17);
        body+=exp(-abs(d)/(7.0*s))*energy;
        core+=exp(-pow(d/(1.4*s),2.0))*energy;
        haze+=exp(-pow((d+8.0*s)/(20.0*s),2.0))*energy*0.12;
    }
    let border=abs(box(q,text.text_size*0.5+vec2f(5,1),3.0));
    core+=exp(-border/(0.8*s))*pulse(t)*0.55;
    return paint(body,core,haze);
}
fn converge(p:vec2f,t:f32)->vec4f {
    let center=text.text_size*0.5;let q=p-center;let s=text.effect_scale*sqrt(params.intensity);
    let half=text.text_size*0.5+vec2f(9,5);
    var body=0.0;var core=0.0;var haze=0.0;
    for(var i=0u;i<4u;i++) {
        let side=vec2f(select(-1.0,1.0,i%2u==0u),select(-1.0,1.0,i<2u));
        let v=q*side;let x=max(v.x-half.x,0.0);
        let edges=select(center-text.paint_offset,text.paint_offset+text.surface-center,side>vec2f(0));
        let travel=max(edges.x-half.x,8.0);let bend=max(edges.y-half.y,8.0)*0.85;
        let u=x/travel;
        let y=half.y+u*u*bend;
        let slope=2.0*u*bend/travel;
        let d=abs(v.y-y)/sqrt(1.0+slope*slope);
        let head=(1.0-ease(f32(i)*0.018,0.67,t))*travel;
        let behind=x-head;let ribbon=smoothstep(-3.0*s,3.0*s,behind)*exp(-max(behind,0.0)/(travel*0.50));
        let visible=step(half.x,v.x)*pulse(t)*(1.0-ease(0.62,0.78,t));
        body+=exp(-pow(d/(7.5*s),2.0))*ribbon*visible;
        core+=exp(-pow(d/(1.0*s),2.0))*ribbon*visible;
        haze+=exp(-pow(d/(16.0*s),2.0))*ribbon*visible*0.10;
        let headPoint=vec2f(half.x+head,half.y+pow(head/travel,2.0)*bend);
        core+=exp(-dot(v-headPoint,v-headPoint)/(20.0*s*s))*visible;
    }
    let lock=ease(0.46,0.68,t)*(1.0-ease(0.78,1.0,t));
    let v=abs(q);let ends=segment(v,vec2f(half.x,0),half);
    let shoulders=segment(v,half,half-vec2f(14.0*s,0));
    core+=exp(-min(ends,shoulders)/(1.25*s))*lock;
    haze+=exp(-abs(box(q,half,4.0))/(10.0*s))*lock*0.16;
    return paint(body,core,haze);
}
fn beacon(p:vec2f,t:f32)->vec4f {
    let center=text.text_size*0.5;let q=p-center;let s=text.effect_scale*sqrt(params.intensity);
    let above=center.y-text.paint_offset.y;let below=text.paint_offset.y+text.surface.y-center.y;
    let up=select(q.y,-q.y,above>=below);let height=max(max(above,below),80.0);
    let head=height*1.15*ease(0.0,0.48,t);let life=pulse(t);
    var body=0.0;var core=0.0;var haze=0.0;
    let width=(8.0+max(up,0.0)*0.22)*s;
    if up>-6.0*s && up<head+30.0*s && abs(q.x)<width*4.0+max(up,0.0)*0.21 {
        let gate=smoothstep(-6.0*s,8.0*s,up)*(1.0-smoothstep(head,head+30.0*s,up))*life;
        let grain=0.75+noise3D(vec3f(q/vec2f(17.0*s,80.0*s),t*0.7),1.0)*0.25;
        body=exp(-pow(q.x/max(width,1.0),2.0))*gate*grain*0.80;
        haze=exp(-pow(q.x/max(width*1.8,1.0),2.0))*gate*0.18;
        for(var i=0u;i<5u;i++) {
            let slope=(f32(i)-2.0)*0.105;
            let distance=abs(q.x-up*slope);
            let widthCore=select(0.9,2.0,i==2u)*s;
            let flicker=0.85+0.15*sin(t*6.0+f32(i)*1.7);
            core+=exp(-distance/widthCore)*gate*flicker*select(0.40,0.85,i==2u);
        }
        let cap=exp(-pow((up-head)/(8.0*s),2.0))*exp(-pow(q.x/(width*1.4),2.0));
        core+=cap*life*0.6;
    }
    let radius=14.0+ease(0.08,0.65,t)*65.0*s;
    let ground=length(q/vec2f(1.8,0.28));
    core+=exp(-abs(ground-radius)/(1.6*s))*life*0.7;
    let underline=segment(q,vec2f(-text.text_size.x*0.5,text.text_size.y*0.5+2.0),text.text_size*0.5+vec2f(0,2));
    core+=exp(-underline/(1.2*s))*life*0.7;
    return paint(body,core,haze);
}
fn cometCall(p:vec2f,t:f32)->vec4f {
    let center=text.text_size*0.5;let q=p-center;let s=text.effect_scale*sqrt(params.intensity);
    var body=0.0;var core=0.0;var haze=0.0;
    for(var i=0u;i<2u;i++) {
        let side=select(-1.0,1.0,i==1u);
        let travel=max(select(center.x-text.paint_offset.x,text.paint_offset.x+text.surface.x-center.x,i==1u),30.0);
        let vertical=select(center.y-text.paint_offset.y,text.paint_offset.y+text.surface.y-center.y,i==1u);
        let height=max(vertical*0.70,8.0*s);
        let head=1.0-ease(f32(i)*0.055,0.53+f32(i)*0.055,t);
        let x=q.x*side;let u=clamp(x/travel,0.0,1.0);
        let y=side*(sin(u*3.141593)*0.65+u*0.10)*height;
        let slope=(cos(u*3.141593)*2.042035+0.10)*height/travel;
        let d=abs(q.y-y)/sqrt(1.0+slope*slope);
        let tail=x-head*travel;
        let gate=step(0.0,x)*smoothstep(-4.0*s,2.0*s,tail)*exp(-max(tail,0.0)/(travel*0.42));
        let life=pulse(t)*(1.0-ease(0.52,0.64,t));
        body+=exp(-pow(d/(4.0*s),2.0))*gate*life;
        core+=exp(-pow(d/(1.0*s),2.0))*gate*life;
        haze+=exp(-pow(d/(15.0*s),2.0))*gate*life*0.16;
        let headPoint=vec2f(side*head*travel,side*(sin(head*3.141593)*0.65+head*0.10)*height);
        let delta=q-headPoint;
        core+=exp(-dot(delta,delta)/(25.0*s*s))*life;
        haze+=exp(-dot(delta,delta)/(220.0*s*s))*life*0.35;
    }
    let impact=ease(0.46,0.56,t)*(1.0-ease(0.65,0.96,t));
    let age=clamp((t-0.51)/0.45,0.0,1.0);let radius=10.0+ease(0.0,1.0,age)*reach(center)*0.80;
    let d=length(q)-radius;
    body+=exp(-abs(d)/(6.0*s))*impact;
    core+=exp(-pow(d/(1.2*s),2.0))*impact;
    let flash=exp(-pow((t-0.55)/0.055,2.0));
    core+=exp(-abs(q.y)/(1.0*s))*exp(-abs(q.x)/(text.text_size.x*0.9+40.0))*flash;
    haze+=exp(-dot(q,q)/(500.0*s*s))*flash*0.35;
    return paint(body,core,haze);
}
fn prismSweep(p:vec2f,t:f32)->vec4f {
    let center=text.text_size*0.5;let q=p-center;let s=text.effect_scale*sqrt(params.intensity);
    let r=length(q);let angle=atan2(q.y,q.x);
    let turn=mix(-2.85,1.45,ease(0.0,0.86,t));
    let a=atan2(sin(angle-turn),cos(angle-turn));
    let opening=0.25*sqrt(s);let life=pulse(t);
    let near=smoothstep(8.0*s,22.0*s,r);let reachFade=exp(-r/max(reach(center)*1.1,1.0));
    let fan=pow(max(1.0-abs(a)/opening,0.0),0.55)*near*life*reachFade;
    let stripes=0.78+0.22*pow(max(cos(a/opening*18.0),0.0),4.0);
    let edge=exp(-pow((a-opening*0.68)*r/(1.8*s),2.0))*near*life*reachFade;
    let trailAngle=atan2(sin(angle-turn+0.20),cos(angle-turn+0.20));
    let trail=exp(-pow(trailAngle/0.05,2.0))*near*life*reachFade*0.24;
    let opposite=atan2(sin(angle-turn-3.141593),cos(angle-turn-3.141593));
    let reflected=exp(-pow(opposite/(opening*0.45),2.0))*near*life*reachFade*0.24;
    let tint=clamp(a/opening*0.5+0.5,0.0,1.0);
    var shade=mix(params.base,params.bright,smoothstep(0.0,0.6,tint));
    shade=mix(shade,params.accent,smoothstep(0.65,1.0,tint));
    shade=mix(shade,params.accent,clamp(edge,0.0,1.0));
    let light=smoothstep(0.3,0.8,dot(text.background.rgb,vec3f(0.2126,0.7152,0.0722)));
    let colour=mix(shade.rgb,params.base.rgb*0.48,light*0.85);
    let halo=exp(-r/(15.0*s))*life*0.45;
    let border=exp(-abs(box(q,text.text_size*0.5+vec2f(6,3),4.0))/(1.0*s))*life*0.4;
    let alpha=clamp(fan*stripes*0.65+edge*0.80+trail+reflected+halo+border,0.0,0.9)*min(params.intensity,1.0)*shade.a;
    return vec4f(colour*alpha,alpha);
}
fn alarm(p:vec2f,t:f32)->vec4f {
    let q=p-text.text_size*0.5;let s=text.effect_scale*sqrt(params.intensity);
    let life=pulse(t);let spread=ease(0.0,0.22,t);
    let lane=text.text_size.y*0.5+10.0*s+spread*25.0*s;
    let y=abs(q.y)-lane;let side=select(-1.0,1.0,q.y>0.0);
    let band=1.0-smoothstep(8.0*s,10.0*s,abs(y));
    let march=fract((q.x*side+y*0.9-t*200.0*s)/(48.0*s));
    let stripe=smoothstep(0.36,0.42,march)*(1.0-smoothstep(0.90,0.96,march));
    let beat=0.78+0.22*pow(0.5+0.5*cos(t*12.56637),2.0);
    let body=band*(0.18+stripe*0.82)*life*beat;
    var core=exp(-abs(abs(y)-9.0*s)/(0.9*s))*life*0.65;
    let end=abs(q.x)-text.text_size.x*0.5-7.0*s;
    let bracket=segment(vec2f(end,abs(q.y)),vec2f(0,0),vec2f(0,text.text_size.y*0.5+3.0));
    core+=exp(-bracket/(1.4*s))*life;
    let haze=exp(-pow(y/(25.0*s),2.0))*life*0.13;
    return paint(body,core,haze);
}
fn echoFrame(p:vec2f,t:f32)->vec4f {
    let center=text.text_size*0.5;let q=p-center;let s=text.effect_scale*sqrt(params.intensity);
    let start=text.text_size*0.5+vec2f(8,5);
    let edges=max(center-text.paint_offset,text.paint_offset+text.surface-center);
    var body=0.0;var core=0.0;var haze=0.0;
    for(var i=0u;i<3u;i++) {
        let age=clamp((t-f32(i)*0.15)/0.70,0.0,1.0);
        let grow=ease(0.0,1.0,age);let half=mix(start,edges*1.25,grow);
        let round=8.0*s+grow*32.0*s;
        let d=abs(box(q,half,round));let life=pulse(age)*(1.0-f32(i)*0.17);
        let corner=exp(-abs(abs(q.x)-half.x)/(56.0*s))*exp(-abs(abs(q.y)-half.y)/(56.0*s));
        let sweep=0.65+0.35*sin(atan2(q.y,q.x)*2.0-t*8.0+f32(i));
        body+=exp(-pow(d/(5.0*s),2.0))*life*(0.42+corner*0.58);
        core+=exp(-pow(d/(1.1*s),2.0))*life*(0.28+corner*0.72)*sweep;
        haze+=exp(-pow(d/(15.0*s),2.0))*life*0.13;
    }
    let source=abs(box(q,start,4.0));
    core+=exp(-source/(0.9*s))*pulse(t)*0.6;
    return paint(body,core,haze);
}
fn vitalSign(p:vec2f,t:f32)->vec4f {
    let center=text.text_size*0.5;let q=p-center;let s=text.effect_scale*sqrt(params.intensity);
    let left=text.paint_offset.x-center.x;let right=left+text.surface.x;
    let period=max(160.0*s,text.text_size.x*0.68);
    let u=(q.x+period*0.5)/period-floor((q.x+period*0.5)/period)-0.5;
    let wave=0.13*exp(-pow((u+0.22)/0.045,2.0))
        -0.18*exp(-pow((u+0.055)/0.022,2.0))
        +exp(-pow(u/0.022,2.0))
        -0.38*exp(-pow((u-0.06)/0.03,2.0))
        +0.22*exp(-pow((u-0.24)/0.075,2.0));
    let slope=select(1.0,2.0,abs(u)<0.09);
    let below=text.paint_offset.y+text.surface.y-center.y;
    let above=center.y-text.paint_offset.y;
    let baseline=select(text.text_size.y*0.5+18.0*s,-text.text_size.y*0.5-18.0*s,below<90.0*s && above>below);
    let y=baseline-wave*62.0*s;
    let d=abs(q.y-y)/slope;
    let head=mix(left-20.0*s,right+60.0*s,ease(0.0,0.85,t));
    let behind=head-q.x;let life=pulse(t);
    let tail=smoothstep(-2.0*s,2.0*s,behind)*exp(-max(behind,0.0)/max(text.surface.x*0.48,1.0))*life;
    let body=exp(-pow(d/(4.0*s),2.0))*tail;
    var core=exp(-pow(d/(0.9*s),2.0))*tail;
    let lead=exp(-pow(behind/(7.0*s),2.0))*exp(-pow(d/(3.0*s),2.0))*life;
    core+=lead;
    var haze=exp(-pow(d/(14.0*s),2.0))*tail*0.20;
    haze+=exp(-pow(behind/(16.0*s),2.0)-pow(d/(12.0*s),2.0))*life*0.28;
    let heart=ease(0.28,0.34,t)*(1.0-ease(0.35,0.48,t))+ease(0.53,0.59,t)*(1.0-ease(0.60,0.73,t));
    let underline=segment(q,vec2f(-text.text_size.x*0.5,text.text_size.y*0.5+2.0),text.text_size*0.5+vec2f(0,2));
    core+=exp(-underline/(1.4*s))*heart;
    return paint(body,core,haze);
}
fn effect(p:vec2f)->vec4f {
    if params.intensity<=0.0 || params.speed<=0.0 { return vec4f(0); }
    let t=phase();
    if t<=0.0 || t>=1.0 { return vec4f(0); }
    var signal=vec4f(0);
    if params.variant<0.5 { signal=sonar(p,t); }
    else if params.variant<1.5 { signal=converge(p,t); }
    else if params.variant<2.5 { signal=beacon(p,t); }
    else if params.variant<3.5 { signal=cometCall(p,t); }
    else if params.variant<4.5 { signal=prismSweep(p,t); }
    else if params.variant<5.5 { signal=alarm(p,t); }
    else if params.variant<6.5 { signal=echoFrame(p,t); }
    else { signal=vitalSign(p,t); }
    // Preserve a narrow background-coloured contour around the marked ink.
    if signal.a>0.0 && all(p>=vec2f(-3)) && all(p<=text.text_size+vec2f(3)) {
        let hull=max(max(coverage(p+vec2f(1.5,0)),coverage(p-vec2f(1.5,0))),
            max(coverage(p+vec2f(0,1.5)),coverage(p-vec2f(0,1.5))));
        signal*=1.0-hull*0.94;
    }
    return signal;
}

// Projected impressions of captured ink. The native selectable line stays on top.
// Three bounded meshes per glyph; no pane-sized texture filtering or extra captures.
const TEXT_FRAGMENTS: u32 = 3u;
struct Parameters { intensity: f32, speed: f32, variant: f32, amplitude: f32,
    base: vec4f, bright: vec4f, accent: vec4f }
@group(1) @binding(0) var<uniform> params: Parameters;
fn ksPhase() -> f32 {
    return select(fract(text.time * params.speed / 2.4), text.progress, text.duration > 0.0);
}
fn ksEase(x: f32) -> f32 { let t=clamp(x,0.0,1.0); return t*t*t*(t*(t*6.0-15.0)+10.0); }
fn ksPower() -> f32 { return min(params.intensity*params.amplitude*text.effect_scale,3.0); }
fn ksHash(n: f32) -> f32 { return fract(sin(n*127.1+text.seed)*43758.5453); }
fn effect(p: vec2f) -> vec4f { return vec4f(0); }

// Shared by both stages so native ink can be protected at the projected position.
fn ksPose(source: vec2f, instance: u32) -> SmudgyFragment {
    let phase=ksPhase(); let power=ksPower(); let piece=instance%TEXT_FRAGMENTS;
    if phase<=0.0 || phase>=1.0 || power<=0.0 || params.speed<=0.0 {
        return SmudgyFragment(vec2f(0),source,1.0,0.0);
    }
    let n=f32(piece); let age=phase-n*0.055;
    let multiline=glyphCount()>0u && glyphAt(glyphCount()-1u).cluster.z!=glyphAt(0u).cluster.z;
    let center=vec2f(text.text_size.x*0.5,
        select(textBaseline()-text.text_size.y*0.36,text.text_size.y*0.5,multiline));
    var pivot=center;
    let fit=max(1.0,min(5.5,(text.surface.x-24.0)/max(text.text_size.x+4.0,1.0)));
    var zoom=1.0; var stretch=vec2f(1); var offset=vec2f(0); var angle=0.0; var yaw=0.0;
    var opacity=0.0;
    if params.variant<0.5 {
        // Two blunt warning stamps split into empty outlines, like a lost grip.
        let beat=select(0.065,0.31,piece==1u);
        let a=phase-beat;
        let hit=ksEase(a/0.045)*(1.0-ksEase((a-0.12)/0.40));
        let travel=ksEase(max(a,0.0)/0.48);
        zoom=1.0+(fit-1.0)*(0.70+travel*0.30)*power;
        offset.x=select(-1.0,1.0,piece==1u)*travel*text.text_size.y*2.4*power;
        angle=select(-0.065,0.065,piece==1u)*travel;
        opacity=hit;
        if piece==2u { opacity=0.0; }
    } else if params.variant<1.5 {
        // A broad near-camera impact retreats, leaving progressively thinner depth planes.
        let flight=ksEase((age-0.035)/0.64);
        let attack=ksEase(age/0.04);
        zoom=1.0+(fit-1.0)*(1.0-flight)*power;
        stretch=vec2f(1.0,1.0+0.38*(1.0-flight));
        yaw=-0.36*(1.0-flight);
        offset=vec2f(text.text_size.y*(3.0+n*1.1)*flight,-text.text_size.y*0.5*flight)*power;
        angle=-0.04*(1.0-flight);
        opacity=attack*(1.0-ksEase((age-0.35)/0.42))*(1.0-n*0.22);
    } else if params.variant<2.5 {
        // Two slices of the actual inscription cross along opposed cut planes.
        let a=phase-0.035; let travel=ksEase(a/0.72);
        let side=select(-1.0,1.0,piece==1u);
        zoom=1.0+(fit-1.0)*0.86*power;
        angle=-0.12;
        offset=vec2f(side*(travel-0.45)*text.text_size.x*0.70,
            side*(travel-0.45)*text.text_size.y*2.2)*power;
        opacity=ksEase(a/0.055)*(1.0-ksEase((a-0.44)/0.32));
        if piece==2u { zoom=1.0+(fit-1.0)*ksEase(a/0.30)*power; offset=vec2f(0); angle=0.12;
            opacity=ksEase((a-0.12)/0.06)*(1.0-ksEase((a-0.26)/0.38))*0.38; }
    } else if params.variant<3.5 {
        // Pressure propagates through the letter contours rather than a circular ring.
        let a=phase-n*0.115;
        let spread=ksEase(a/0.72);
        zoom=1.0+(fit-1.0)*spread*power;
        stretch=vec2f(1.0,1.0+0.90*spread);
        opacity=ksEase(a/0.055)*(1.0-ksEase((a-0.33)/0.43))*(1.0-n*0.12);
    } else if params.variant<4.5 {
        // A camera rush: long thin distant type blooms toward the viewer in successive planes.
        let a=phase-n*0.095;
        let rush=ksEase(a/0.68);
        zoom=1.0+(fit-1.0)*rush*power;
        stretch=vec2f(1.0+0.12*(1.0-rush),0.16+1.25*rush);
        yaw=(0.72-0.85*rush)*power*0.65;
        angle=0.07*(1.0-rush);
        offset.x=(rush-0.5)*text.text_size.y*(4.0+n*1.2)*power;
        opacity=ksEase(a/0.045)*(1.0-ksEase((a-0.38)/0.28))*(1.0-n*0.22);
    } else if params.variant<5.5 {
        // Three clean deflections, each with its own impact direction and bank.
        let a=phase-n*0.21; let flight=ksEase(a/0.36);
        let side=select(-1.0,1.0,piece==1u);
        zoom=1.0+(fit-1.0)*(0.82-0.22*flight)*power;
        angle=side*(0.22-0.38*flight);
        yaw=-side*0.35*(1.0-flight);
        offset=vec2f(side*text.text_size.y*(flight*6.0-1.6),
            (n-1.0)*text.text_size.y*0.60+sin(flight*3.141593)*text.text_size.y*0.7)*power;
        opacity=ksEase(a/0.035)*(1.0-ksEase((a-0.12)/0.25));
    } else if params.variant<6.5 {
        // One smooth elastic recovery with damped depth echoes; the real line never bobs.
        let a=max(age,0.0); let attack=ksEase(age/0.08);
        let breathe=sin(clamp(a/0.76,0.0,1.0)*3.141593);
        let recover=1.0-ksEase((a-0.44)/0.38);
        zoom=1.0+(fit-1.0)*breathe*power;
        stretch=vec2f(1.0-0.14*breathe,1.0+0.30*breathe);
        yaw=sin(a*6.283185)*0.38*recover;
        angle=sin(a*6.283185)*0.035*recover;
        offset.x=sin(a*6.283185)*text.text_size.y*(1.3+n*0.8)*power;
        opacity=attack*recover*(1.0-n*0.28);
    } else if params.variant<7.5 {
        // Separate oversized glyph contours turn into alignment and lock onto their source.
        let glyphIndex=instance/TEXT_FRAGMENTS;
        let glyph=glyphAt(glyphIndex);
        pivot=vec2f(glyph.advance.x+glyph.advance.z*0.5,glyphBaseline(glyphIndex)-glyph.advance.w*0.36);
        let a=phase-n*0.055;
        let close=ksEase((a-0.05)/0.65); let residual=1.0-close;
        zoom=1.0+(fit-1.0)*(0.70+n*0.15)*residual*power;
        stretch.y=1.0+0.6*residual;
        angle=(ksHash(f32(glyphIndex)+3.0)-0.5)*0.7*residual;
        yaw=(ksHash(f32(glyphIndex)+11.0)-0.5)*1.6*residual;
        offset.x=(pivot.x-center.x)*residual*0.12*power;
        opacity=ksEase(a/0.045)*(1.0-ksEase((a-0.52)/0.24))*(1.0-n*0.16);
    } else if params.variant<8.5 {
        // A single oblique stamp, with shallow extruded ink and a polished broad reflection.
        let a=phase-0.045; let slam=ksEase(a/0.17);
        zoom=1.0+(fit-1.0)*(0.97-0.18*slam)*power;
        angle=-0.095;
        yaw=-0.36*(1.0-slam);
        stretch.y=1.0+0.28*(1.0-slam);
        offset=vec2f(n*1.8,-text.text_size.y*1.3*(1.0-slam)+n*1.3)*power;
        opacity=ksEase(a/0.055)*(1.0-ksEase((a-0.52)/0.28))*select(0.85,0.45,piece>0u);
    } else {
        // Opposed blood-red planes close in; one final contour pulse holds the source in focus.
        let side=select(-1.0,1.0,piece==1u);
        let close=ksEase((phase-0.07)/0.45);
        let release=1.0-ksEase((phase-0.60)/0.30);
        zoom=1.0+(fit-1.0)*0.84*power;
        stretch.y=1.15-0.42*close;
        yaw=side*0.52*(1.0-close);
        angle=side*0.07*(1.0-close);
        offset.y=side*text.text_size.y*(2.2-2.0*close)*power;
        opacity=ksEase((phase-0.015)/0.07)*release*0.9;
        if piece==2u {
            let pulse=sin(clamp((phase-0.34)/0.45,0.0,1.0)*3.141593);
            zoom=1.0+(fit-1.0)*pulse*power; offset=vec2f(0); yaw=0.0; angle=0.0;
            stretch=vec2f(1.0,1.0+0.45*pulse); opacity=pulse*0.85;
        }
    }
    let local=(source-pivot)*stretch;
    let turned=vec2f(local.x*cos(angle)-local.y*sin(angle),local.x*sin(angle)+local.y*cos(angle));
    let camera=max(text.text_size.x*1.2,160.0);
    let w=max(0.5,1.0-turned.x*sin(yaw)/camera);
    // Fit the impression's center to the pane. The source anchor itself never moves.
    let halfWidth=(text.text_size.x*0.5+2.0)*zoom*stretch.x
        +text.text_size.y*zoom*abs(sin(angle))*stretch.y;
    let low=text.paint_offset.x+halfWidth+6.0;
    let high=text.paint_offset.x+text.surface.x-halfWidth-6.0;
    let fitted=select(text.paint_offset.x+text.surface.x*0.5,clamp(center.x+offset.x,low,max(low,high)),low<=high);
    let destination=vec2f(fitted,center.y+offset.y);
    let position=destination+(pivot-center+vec2f(turned.x*cos(yaw),turned.y))*zoom/w;
    return SmudgyFragment(position,source,w,opacity*min(params.intensity,1.0));
}
fn textVertex(instance: u32, corner: vec2f) -> SmudgyFragment {
    let glyphIndex=instance/TEXT_FRAGMENTS;
    if glyphsTruncated() && glyphIndex>0u { return SmudgyFragment(vec2f(0),vec2f(0),1.0,0.0); }
    let glyph=glyphAt(glyphIndex);
    var lo=glyph.advance.xy-vec2f(0,2);
    var hi=glyph.advance.xy+glyph.advance.zw+vec2f(0,2);
    let first=glyphIndex==0u || glyphAt(glyphIndex-1u).cluster.z!=glyph.cluster.z;
    let last=glyphIndex+1u==glyphCount() || glyphAt(glyphIndex+1u).cluster.z!=glyph.cluster.z;
    if !last { hi.x=glyphAt(glyphIndex+1u).advance.x; }
    if first { lo.x=min(lo.x,glyph.ink.x)-2.0; }
    if last { hi.x=max(hi.x,glyph.ink.x+glyph.ink.z)+2.0; }
    if glyphsTruncated() { lo=vec2f(-2); hi=text.text_size+vec2f(2); }
    if params.variant>=1.5 && params.variant<2.5 {
        let middle=glyphBaseline(glyphIndex)-glyph.advance.w*0.36;
        if instance%TEXT_FRAGMENTS==0u { hi.y=middle; }
        if instance%TEXT_FRAGMENTS==1u { lo.y=middle; }
    }
    return ksPose(mix(lo,hi,corner),instance);
}
fn textFragment(color: vec4f, source: vec2f, instance: u32) -> vec4f {
    let phase=ksPhase(); let pose=ksPose(source,instance);
    let piece=instance%TEXT_FRAGMENTS;
    // Inner contour stays inside the source cell, avoiding seams at glyph boundaries.
    let radius=0.45;
    let inner=min(min(coverage(source+vec2f(radius,0)),coverage(source-vec2f(radius,0))),
        min(coverage(source+vec2f(0,radius)),coverage(source-vec2f(0,radius))));
    let edge=max(color.a-inner,0.0);
    let beat=select(0.065,0.31,piece==1u);
    var shell=ksEase((phase-beat-0.07)/0.15);
    if params.variant>=0.5 { shell=clamp(f32(piece)*0.44+phase*0.75,0.0,1.0); }
    if params.variant>=1.5 && params.variant<2.5 { shell=select(0.25,1.0,piece==2u); }
    if params.variant>=2.5 && params.variant<3.5 { shell=1.0; }
    if params.variant>=3.5 && params.variant<4.5 { shell=clamp(f32(piece)*0.65,0.0,1.0); }
    if params.variant>=4.5 && params.variant<5.5 { shell=ksEase(fract(phase*3.0)/0.65)*0.8; }
    if params.variant>=5.5 && params.variant<6.5 { shell=select(0.15,1.0,piece>0u); }
    if params.variant>=6.5 && params.variant<7.5 { shell=1.0; }
    if params.variant>=7.5 && params.variant<8.5 { shell=select(0.05,0.0,piece>0u); }
    if params.variant>=8.5 { shell=select(0.30,1.0,piece==2u); }
    var ink=mix(color.a*0.82,edge*2.8,shell);
    if params.variant>=2.5 && params.variant<3.5 { ink=edge*5.0; }
    if params.variant>=6.5 && params.variant<7.5 { ink=edge*4.5; }
    if params.variant>=8.5 && piece==2u { ink=edge*4.5; }
    let stripe=0.5+0.5*sin((source.x-source.y*2.0)*0.31-phase*16.0*params.speed);
    let glint=pow(stripe,12.0);
    var tint=mix(params.base.rgb,params.bright.rgb,0.75+glint*0.25);
    let impact=exp(-max(phase-beat-0.035,0.0)*25.0);
    tint=mix(tint,params.accent.rgb,max(glint*0.65,impact*0.90));
    if params.variant>=2.5 && params.variant<3.5 { tint=mix(params.bright.rgb,params.accent.rgb,0.55); }
    if params.variant>=3.5 && params.variant<4.5 {
        let sweep=pow(0.5+0.5*cos(source.x/max(text.text_size.x,1.0)*9.0-phase*22.0*params.speed),6.0);
        tint=mix(params.bright.rgb,params.accent.rgb,sweep);
    }
    if params.variant>=7.5 && params.variant<8.5 {
        let reflection=exp(-pow((source.x/max(text.text_size.x,1.0)-phase*1.8*params.speed+0.2)/0.22,2.0));
        let bevel=clamp((coverage(source-vec2f(0,0.5))-coverage(source+vec2f(0,0.5)))*1.5+0.5,0.0,1.0);
        tint=mix(params.base.rgb,params.bright.rgb,0.35+0.65*bevel);
        tint=mix(tint,params.accent.rgb,reflection*0.95);
        if piece>0u { tint=params.base.rgb*(0.65+f32(piece)*0.12); }
    }
    // Contrast on a light terminal: retain the palette's darker ink instead of white glare.
    let luminance=dot(text.background.rgb,vec3f(0.2126,0.7152,0.0722));
    tint=mix(tint,mix(params.base.rgb,params.bright.rgb,0.2),smoothstep(0.35,0.85,luminance)*0.75);
    // Leave a background-coloured stroke around the real, unmoved terminal letters.
    var guard=0.0;
    let p=pose.position;
    if all(p>=vec2f(-3)) && all(p<=text.text_size+vec2f(3)) {
        guard=max(max(coverage(p+vec2f(1.1,0)),coverage(p-vec2f(1.1,0))),
            max(coverage(p+vec2f(0,1.1)),coverage(p-vec2f(0,1.1))));
        guard=max(guard,coverage(p));
    }
    let paletteAlpha=mix(params.base.a,params.bright.a,0.6);
    let alpha=clamp(ink*paletteAlpha,0.0,0.85);
    return vec4f(mix(tint,text.background.rgb,guard)*alpha,alpha);
}

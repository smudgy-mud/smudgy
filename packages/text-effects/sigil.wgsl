// Breaking magic and a prismatic divine shield use the same bounded geometry.
struct Parameters { intensity: f32, speed: f32, variant: f32, base: vec4f, bright: vec4f, accent: vec4f }
@group(1) @binding(0) var<uniform> params: Parameters;
fn hash(n: f32) -> f32 { return fract(sin(n * 127.1 + 311.7) * 43758.5453); }
fn segment(p: vec2f, a: vec2f, b: vec2f) -> f32 {
 let v = b-a; return length(p-a-v*clamp(dot(p-a,v)/max(dot(v,v),0.001),0.0,1.0));
}
fn tint(ink: f32, glow: f32) -> vec4f {
 var c = mix(params.base, params.bright, clamp(ink,0.0,1.0));
 c = mix(c,params.accent,smoothstep(0.5,1.0,ink));
 let light = smoothstep(0.35,0.8,dot(text.background.rgb,vec3f(0.2126,0.7152,0.0722)));
 c = mix(c,params.base,light*0.6);
 let a = clamp((ink+glow)*params.intensity,0.0,1.0)*c.a; return vec4f(c.rgb*a,a);
}
fn effect(p: vec2f) -> vec4f {
 let s=text.effect_scale;
 let q=(p-text.text_size*0.5)/s;
 let halfWidth=text.text_size.x*0.5/s;
 let halfHeight=text.text_size.y*0.5/s;
 let phase=select(fract(text.time*params.speed/2.0),text.progress,text.duration>0.0);
 var ink=0.0; var glow=0.0;
 if params.variant < 0.5 {
   // The seal fractures into twelve independent, shrinking runic shards.
   let age=smoothstep(0.20,0.94,phase);
   let radius=vec2f(halfWidth+8.0,halfHeight+5.0);
   for(var i=0;i<12;i++) {
     let id=f32(i)+text.seed;
     let angle=f32(i)*0.523599;
     let direction=vec2f(cos(angle),sin(angle));
     let offset=direction*age*(12.0+hash(id)*22.0);
     let centre=radius*direction+offset;
     let tangent=vec2f(-direction.y,direction.x);
     let r=q-centre;
     let d=min(segment(r,-tangent*6.0,tangent*6.0),segment(r,tangent*-2.0,tangent*2.0+direction*5.0));
     let alive=smoothstep(0.0,0.10,phase)*pow(1.0-age,0.6);
     ink+=exp(-d*d/0.6)*alive; glow+=exp(-d*d/14.0)*alive*0.25;
   }
   let rim=max(coverage(p+vec2f(s,0.0)),coverage(p-vec2f(s,0.0)))*(1.0-coverage(p));
   ink+=rim*sin(phase*3.141593)*0.6;
 } else {
   // A low capsule catches a moving highlight; facets stay close to the text.
   let build=smoothstep(0.0,0.23,phase);
   let stretch=mix(0.2,1.0,build);
   let h=halfHeight+4.0;
   let body=max(halfWidth-h+8.0,0.0)*stretch;
   let v=vec2f(max(abs(q.x)-body,0.0),q.y);
   let d=abs(length(v)-h);
   let inner=abs(length(v)-(h-3.0));
   let angle=atan2(q.y,q.x/max(halfWidth,1.0)*h);
   let highlight=pow(max(cos(angle-text.time*params.speed*1.4),0.0),14.0);
   ink=(exp(-d*d/0.65)*(0.5+highlight*0.6)+exp(-inner*inner/0.35)*0.20)*build;
   glow=exp(-d*d/18.0)*build*0.17;
   for(var i=0;i<6;i++) {
     let x=(f32(i)/5.0-0.5)*(body*2.0);
     let diamond=abs(q.x-x)/3.0+abs(abs(q.y)-h)/3.0;
     ink+=(1.0-smoothstep(0.7,1.1,diamond))*build*(0.25+0.35*highlight);
   }
 }
 return tint(ink,glow);
}

struct Parameters { intensity: f32, speed: f32, base: vec4f, bright: vec4f, accent: vec4f }
@group(1) @binding(0) var<uniform> params: Parameters;
fn hash(n: f32) -> f32 {return fract(sin(n*127.1+311.7)*43758.5453);}
fn segment(p: vec2f,a: vec2f,b: vec2f) -> f32 {let v=b-a;return length(p-a-v*clamp(dot(p-a,v)/max(dot(v,v),0.001),0.0,1.0));}
fn effect(p: vec2f) -> vec4f {
 let s=text.effect_scale;let q=(p-text.text_size*0.5)/s;
 let radius=vec2f(text.text_size.x*0.5/s+12.0,text.text_size.y*0.5/s+8.0);
 let orbit=abs(length(q/radius)-1.0)*min(radius.x,radius.y);
 let angle=atan2(q.y/radius.y,q.x/radius.x);
 let tracer=pow(max(cos(angle-text.time*params.speed*0.5),0.0),12.0);
 var ink=exp(-orbit*orbit/0.3)*(0.10+0.3*tracer);
 var glow=exp(-orbit*orbit/8.0)*tracer*0.10;
 for(var i=0;i<10;i++) {
   let id=f32(i)+text.seed;
   let a=f32(i)*0.628319+text.time*params.speed*select(0.24,-0.17,i%2==0);
   let centre=radius*vec2f(cos(a),sin(a));
   let r=q-centre;
   if abs(r.x)>9.0 || abs(r.y)>9.0 {continue;}
   // Seeded combinations of strokes are actual drawn rune shapes, not font glyphs.
   let d0=segment(r,vec2f(0.0,-4.0),vec2f(0.0,4.0));
   let d1=segment(r,vec2f(0.0,-4.0),vec2f(3.0,-1.0));
   let d2=segment(r,vec2f(3.0,-1.0),vec2f(0.0,1.0));
   let d3=segment(r,vec2f(0.0,-1.0),vec2f(-3.0,select(2.0,-3.0,hash(id)>0.5)));
   let d=min(min(d0,d1),min(d2,d3));
   let pulse=0.6+0.4*sin(text.time*params.speed+id);
   ink+=exp(-d*d/0.40)*pulse;glow+=exp(-d*d/7.0)*pulse*0.16;
 }
 var c=mix(params.base,params.bright,clamp(ink,0.0,1.0));c=mix(c,params.accent,smoothstep(0.65,1.0,ink));
 let light=smoothstep(0.35,0.8,dot(text.background.rgb,vec3f(0.2126,0.7152,0.0722)));c=mix(c,params.base,light*0.6);
 let alpha=clamp((ink+glow)*params.intensity,0.0,1.0)*c.a;return vec4f(c.rgb*alpha,alpha);
}

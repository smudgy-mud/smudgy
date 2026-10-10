struct Parameters { intensity: f32, speed: f32, particles: f32, base: vec4f, bright: vec4f, accent: vec4f }
@group(1) @binding(0) var<uniform> params: Parameters;
fn hash(n: f32) -> f32 { return fract(sin(n*127.1+311.7)*43758.5453); }
fn effect(p: vec2f) -> vec4f {
 let s=text.effect_scale; var glow=0.0; var body=0.0;
 let count=min(u32(ceil(params.particles*12.0)),24u);
 for(var i=0u;i<count;i++) {
   let id=f32(i)+text.seed;
   let t=text.time*params.speed*(0.55+hash(id)*0.55)+hash(id+1.0)*6.283185;
   let centre=vec2f((f32(i)+0.5)/f32(max(count,1u))*text.text_size.x,text.text_size.y*0.5);
   let orbit=vec2f(sin(t)*12.0,cos(t*0.7)*9.0+sin(t*1.3)*4.0)*s;
   let r=(p-centre-orbit)/s;
   if abs(r.x)>14.0 || abs(r.y)>14.0 {continue;}
   let pulse=pow(max(sin(t*1.9+hash(id+2.0)*6.283185)*0.5+0.5,0.0),2.0);
   let alive=0.07+pulse*0.93;
   body+=exp(-dot(r,r)/0.90)*alive;
   glow+=exp(-dot(r,r)/22.0)*alive*0.27;
   let wing=vec2f(abs(r.x)-1.1,r.y+0.8);
   body+=exp(-dot(wing,wing)/0.45)*alive*0.25;
 }
 var c=mix(params.base,params.bright,clamp(body+glow,0.0,1.0));
 c=mix(c,params.accent,smoothstep(0.1,0.9,body));
 let light=smoothstep(0.35,0.8,dot(text.background.rgb,vec3f(0.2126,0.7152,0.0722)));
 c=mix(c,params.base,light*0.65);
 let a=clamp((body+glow)*params.intensity,0.0,1.0)*c.a; return vec4f(c.rgb*a,a);
}

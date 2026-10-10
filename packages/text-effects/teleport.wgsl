// Thin glyph columns lift away from left to right, then rebuild from right to left.
struct Parameters { intensity: f32, speed: f32, base: vec4f, bright: vec4f, accent: vec4f }
@group(1) @binding(0) var<uniform> params: Parameters;
fn hash(n: f32) -> f32 { return fract(sin(n * 127.1 + 311.7) * 43758.5453); }
fn effect(p: vec2f) -> vec4f {
    if params.intensity <= 0.0 { return sampleText(p); }
    let phase = select(fract(text.time * params.speed / 3.0), text.progress, text.duration > 0.0);
    if phase >= 0.98 { return sampleText(p); }
    let s = text.effect_scale;
    let x = clamp(p.x / max(text.text_size.x, 1.0), 0.0, 1.0);
    let column = floor(p.x / (3.0 * s));
    let jitter = hash(column + text.seed);
    let depart = smoothstep(0.08 + x * 0.23, 0.27 + x * 0.23, phase);
    let arrive = smoothstep(0.53 + (1.0 - x) * 0.22, 0.73 + (1.0 - x) * 0.22, phase);
    let missing = depart * (1.0 - arrive);
    let lift = sin(missing * 1.570796) * (16.0 + jitter * 32.0) * s;
    let displaced = sampleText(p + vec2f(0.0, lift));
    let remnant = displaced * pow(1.0 - missing, 2.0);
    let native = sampleText(p);
    let amount = min(params.intensity, 1.0);
    var result = mix(native, remnant, amount);
    let beamX = (fract(p.x / (3.0 * s)) - 0.5) * 3.0;
    let originalInk = max(coverage(vec2f(p.x, text.text_size.y * 0.35)), coverage(vec2f(p.x, text.text_size.y * 0.65)));
    let transfer = sin(missing * 3.141593);
    let columnGlow = exp(-beamX * beamX * 3.0) * exp(-abs(p.y - text.text_size.y * 0.5 + lift * 0.6) / (18.0 * s));
    let twinkle = 0.65 + 0.35 * sin(text.time * params.speed * 9.0 + column * 2.4);
    var tint = mix(params.base, params.bright, twinkle);
    let light = smoothstep(0.35, 0.8, dot(text.background.rgb, vec3f(0.2126, 0.7152, 0.0722)));
    tint = vec4f(mix(tint.rgb, params.base.rgb, light * 0.6), tint.a);
    let alpha = clamp(columnGlow * originalInk * transfer * 0.55 * params.intensity, 0.0, 0.8) * tint.a;
    result = result + vec4f(tint.rgb * alpha, alpha) * (1.0 - result.a);
    // Small square data fragments travel with the lifted columns, rather than random snow.
    let chipY = text.text_size.y * 0.5 - lift * 0.85;
    let chip = (1.0 - smoothstep(0.7, 1.15, max(abs(beamX), abs((p.y - chipY) / s))));
    let spark = clamp(chip * transfer * originalInk * params.intensity * 0.65, 0.0, 1.0) * params.accent.a;
    let accent = mix(params.accent.rgb, params.base.rgb, light * 0.55);
    result = result + vec4f(accent * spark, spark) * (1.0 - result.a);
    return result;
}

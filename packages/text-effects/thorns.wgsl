struct Parameters { intensity: f32, speed: f32, base: vec4f, bright: vec4f, accent: vec4f }
@group(1) @binding(0) var<uniform> params: Parameters;

fn hash(p: vec2f) -> f32 { return fract(sin(dot(p, vec2f(127.1, 311.7))) * 43758.5453); }
fn over(top: vec4f, bottom: vec4f) -> vec4f { return top + bottom * (1.0 - top.a); }
fn plantColor(amount: f32, light: f32, young: f32) -> vec4f {
    let tint = mix(mix(params.base, params.bright, clamp(light, 0.0, 1.0)), params.accent, young * 0.45);
    let alpha = clamp(amount * params.intensity, 0.0, 1.0) * tint.a;
    return vec4f(tint.rgb * alpha, alpha);
}

// A vine winds around the lettering's ink-height, not around two horizontal borders.
// The third component is projected depth: the same stem passes behind and in front.
fn vine(x: f32, strand: f32) -> vec3f {
    let s = text.effect_scale;
    let height = text.text_size.y;
    let radius = (height * 0.30 + 2.0) * s;
    let wavelength = max(height * 2.75 * s, 44.0 * s);
    let k = 6.2831853 / wavelength;
    let seed = hash(vec2f(text.seed, strand + 2.0));
    let theta = x * k + strand * 2.75 + seed * 0.55
        + 0.28 * sin(x * k * 0.47 + seed * 4.0);
    let derivative = k * (1.0 + 0.1316 * cos(x * k * 0.47 + seed * 4.0));
    let center = textBaseline() - height * 0.30;
    let breadth = 1.0 + 0.10 * sin(x * k * 0.63 + seed * 5.0);
    let slope = radius * (cos(theta) * derivative * breadth + sin(theta) * 0.063 * k * cos(x * k * 0.63 + seed * 5.0));
    return vec3f(center + sin(theta) * radius * breadth, slope, cos(theta));
}

fn effect(p: vec2f) -> vec4f {
    let original = sampleText(p);
    if params.intensity <= 0.0 || params.speed <= 0.0 { return original; }
    let s = text.effect_scale;
    let height = text.text_size.y;
    let center = textBaseline() - height * 0.30;
    if p.x < -23.0 * s || p.x > text.text_size.x + 23.0 * s
        || abs(p.y - center) > (height * 0.34 + 19.0) * s { return original; }
    let grow = select(clamp(text.time * params.speed / 1.8, 0.0, 1.0), text.progress, text.duration > 0.0);
    let advance = smoothstep(0.0, 0.83, grow);
    let left = -6.0 * s;
    let right = text.text_size.x + 6.0 * s;
    let span = right - left;
    let spacing = max(height * 0.68 * s, 13.0 * s);
    let radius = (0.60 + height * 0.024) * s;
    var behind = vec4f(0.0);
    var inFront = vec4f(0.0);

    for (var i = 0; i < 2; i++) {
        let strand = f32(i);
        let direction = select(1.0, -1.0, i == 1);
        let along = select((p.x - left) / span, (right - p.x) / span, i == 1);
        var curve = vine(p.x, strand);
        // One local projection follows steep turns without breaking the stem into horizontal dashes.
        let nearest = p.x + (p.y - curve.x) * curve.y / (1.0 + curve.y * curve.y);
        curve = vine(nearest, strand);
        let distance = (p.y - curve.x) / sqrt(1.0 + curve.y * curve.y);
        let age = advance - clamp(along, 0.0, 1.0);
        let mature = smoothstep(0.0, 0.18, age);
        let thickness = radius * mix(0.16, 1.0, mature);
        let extent = smoothstep(-0.015, 0.008, along) * (1.0 - smoothstep(1.0, 1.025, along));
        let growing = (1.0 - smoothstep(advance - 0.006, advance + 0.005, along)) * step(0.001, advance);
        let body = (1.0 - smoothstep(thickness, thickness + 0.65, abs(distance))) * extent * growing;
        let ridgeDistance = (distance + thickness * 0.35) / max(thickness * 0.32, 0.25);
        let ridge = exp(-ridgeDistance * ridgeDistance);
        var plant = plantColor(body, 0.18 + ridge * 0.55 + mature * 0.10, (1.0 - mature) * 0.40);

        // Only nearby nodes need evaluation. Each sharp hooked thorn remains attached
        // to its stem and grows after that part of the vine has reached it.
        let node = floor((p.x - left) / spacing);
        for (var j = -1; j <= 1; j++) {
            let n = node + f32(j);
            let seed = hash(vec2f(n + strand * 71.0, text.seed + 5.0));
            let x = left + (n + 0.35 + seed * 0.25) * spacing;
            if x < left + 3.0 * s || x > right - 3.0 * s { continue; }
            let at = vine(x, strand);
            let nodeAlong = select((x - left) / span, (right - x) / span, i == 1);
            let thornGrow = smoothstep(nodeAlong * 0.83 + 0.055, nodeAlong * 0.83 + 0.17, grow);
            if thornGrow < 0.002 { continue; }
            let tangent = normalize(vec2f(1.0, at.y));
            let side = select(-1.0, 1.0, at.x >= center);
            let normal = vec2f(-tangent.y, tangent.x) * side;
            let q = p - vec2f(x, at.x);
            let v = vec2f(dot(q, tangent) * direction, dot(q, normal));
            let length = (4.8 + seed * 4.3) * s * thornGrow;
            let t = clamp(v.y / max(length, 0.001), 0.0, 1.0);
            let hook = -length * 0.31 * t * t;
            let width = (1.65 + seed * 0.50) * s * thornGrow * pow(1.0 - t, 1.2);
            let edge = max(abs(v.x - hook) - width, max(-v.y - radius * 0.45, v.y - length));
            let thorn = 1.0 - smoothstep(-0.12, 0.55, edge);
            let spike = plantColor(thorn * 0.98, 0.14 + t * 0.40, smoothstep(0.55, 1.0, t) * 0.65);
            plant = over(spike, plant);
        }

        let front = smoothstep(-0.16, 0.16, curve.z);
        // Rear turns are darker; foreground turns overlap captured letters but
        // keep enough of the original ink to read through each thin crossing.
        let rear = vec4f(plant.rgb * 0.65, plant.a) * (1.0 - front);
        behind = over(rear, behind);
        inFront = over(plant * front, inFront);
    }

    // Two growing spiral tendrils curl around the ends instead of terminating as a border.
    for (var i = 0; i < 2; i++) {
        let side = select(-1.0, 1.0, i == 1);
        let rootX = select(left, right, i == 1);
        let origin = vec2f(rootX - side * 2.0 * s, vine(rootX, f32(i)).x);
        let q = (p - origin) * vec2f(side, 1.0);
        let turn = atan2(q.y, q.x);
        let a = select(turn + 6.2831853, turn, turn >= 0.0);
        let curlGrow = smoothstep(0.03, 0.39, grow);
        let r = 2.0 * s + a * 1.02 * s;
        let d = abs(length(q) - r);
        let gate = (1.0 - smoothstep(curlGrow * 5.8 - 0.18, curlGrow * 5.8, a)) * step(0.001, curlGrow);
        let stem = (1.0 - smoothstep(radius * 0.62, radius * 0.62 + 0.6, d)) * gate;
        behind = over(plantColor(stem * 0.85, 0.35, (1.0 - curlGrow) * 0.45), behind);
    }
    // A narrow background-colored clearance keeps small and italic strokes distinct.
    // It is evaluated only where plant geometry is present, and does not move the text.
    var edge = 0.0;
    if max(inFront.a, behind.a) > 0.01 {
        edge = max(max(coverage(p + vec2f(0.85, 0.0)), coverage(p - vec2f(0.85, 0.0))),
            max(coverage(p + vec2f(0.0, 0.85)), coverage(p - vec2f(0.0, 0.85))));
    }
    let clearance = smoothstep(0.04, 0.70, edge) * (1.0 - original.a) * 0.80;
    behind = mix(behind, vec4f(text.background.rgb * behind.a, behind.a), clearance);
    inFront = mix(inFront, vec4f(text.background.rgb * inFront.a, inFront.a), clearance);
    inFront *= 1.0 - original.a * 0.74;
    return over(inFront, over(original, behind));
}

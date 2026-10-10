// Full-surface adapter for the shared TextEffect contract.
@vertex fn smudgy_vs(@builtin(vertex_index) vertex: u32) -> @builtin(position) vec4f {
    let p = vec2f(f32((vertex << 1u) & 2u), f32(vertex & 2u));
    return vec4f(p * 2.0 - 1.0, 0.0, 1.0);
}
@fragment fn smudgy_fs(@builtin(position) pixel: vec4f) -> @location(0) vec4f {
    let position = (pixel.xy - text.origin) / text.resolution * text.surface + text.paint_offset;
    var color = effect(position);
    // Replacement shaders fade against their original glyphs; underlays fade to transparent.
    var original = vec4f(0.0);
    if text.replaces_text != 0u { original = sampleText(position); }
    color = mix(original, color, text.envelope);
    // Packages return premultiplied sRGB; the host adapts to the target format.
    if text.linearize != 0u && color.a > 0.0 {
        let rgb = color.rgb / color.a;
        color = vec4f(select(rgb / 12.92, pow((rgb + 0.055) / 1.055, vec3f(2.4)), rgb > vec3f(0.04045)) * color.a, color.a);
    }
    return color;
}

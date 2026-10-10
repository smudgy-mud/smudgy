//! CPU layouts uploaded to the WGSL host. Reflection tests verify every field.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct Uniforms {
    pub origin: [f32; 2],
    pub resolution: [f32; 2],
    pub surface: [f32; 2],
    pub texture_size: [f32; 2],
    pub text_size: [f32; 2],
    pub scale: f32,
    pub outset: f32,
    pub time: f32,
    pub progress: f32,
    pub seed: f32,
    pub linearize: u32,
    pub background: [f32; 4],
    pub effect_scale: f32,
    pub duration: f32,
    pub envelope: f32,
    pub replaces_text: u32,
    pub paint_offset: [f32; 2],
    pub capture_offset: [f32; 2],
}
pub(super) const MAX_GLYPHS: usize = 256;
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GlyphGeometry {
    pub advance: [f32; 4],
    pub ink: [f32; 4],
    pub cluster: [u32; 4],
}
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct GlyphTable {
    pub info: [u32; 4],
    pub items: [GlyphGeometry; MAX_GLYPHS],
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytemuck::Zeroable;
    use naga::{Module, ScalarKind, StructMember, TypeInner};
    use std::mem::{offset_of, size_of};

    trait Numeric {
        const KIND: ScalarKind;
        const LANES: usize;
    }
    impl Numeric for f32 {
        const KIND: ScalarKind = ScalarKind::Float;
        const LANES: usize = 1;
    }
    impl Numeric for u32 {
        const KIND: ScalarKind = ScalarKind::Uint;
        const LANES: usize = 1;
    }
    impl<T: Numeric, const N: usize> Numeric for [T; N] {
        const KIND: ScalarKind = T::KIND;
        const LANES: usize = N;
    }

    fn host() -> Module {
        naga::front::wgsl::parse_str(smudgy_session_model::text_shader::COMMON).unwrap()
    }

    fn structure<'a>(module: &'a Module, name: &str, rust_size: usize) -> &'a [StructMember] {
        let ty = module
            .types
            .iter()
            .find(|(_, ty)| ty.name.as_deref() == Some(name))
            .unwrap()
            .1;
        let TypeInner::Struct { members, span } = &ty.inner else {
            panic!("{name} must be a WGSL struct");
        };
        assert_eq!(*span as usize, rust_size, "{name} size");
        members
    }

    fn numeric<T: Numeric>(module: &Module, member: &StructMember, _: &T) {
        let (scalar, lanes) = match module.types[member.ty].inner {
            TypeInner::Scalar(scalar) => (scalar, 1),
            TypeInner::Vector { scalar, size } => (scalar, size as usize),
            _ => panic!("{:?} must be numeric", member.name),
        };
        assert_eq!(scalar.width, 4, "{:?} scalar width", member.name);
        assert_eq!(scalar.kind, T::KIND, "{:?} scalar kind", member.name);
        assert_eq!(lanes, T::LANES, "{:?} vector width", member.name);
    }

    // Infer scalar/vector types from the actual Rust fields, and compare all
    // names, offsets, types and total size against Naga's WGSL reflection.
    macro_rules! numeric_structure {
        ($module:expr, $name:literal, $rust:ty, [$($field:ident),+ $(,)?]) => {{
            let value = <$rust>::zeroed();
            let fields = structure($module, $name, size_of::<$rust>());
            let names = [$(stringify!($field)),+];
            assert_eq!(fields.len(), names.len(), "{} field count", $name);
            let mut fields = fields.iter();
            $(
                let field = fields.next().unwrap();
                assert_eq!(field.name.as_deref(), Some(stringify!($field)));
                assert_eq!(field.offset as usize, offset_of!($rust, $field), "{} offset", stringify!($field));
                numeric($module, field, &value.$field);
            )+
        }};
    }

    #[test]
    fn cpu_frame_matches_the_wgsl_host_contract() {
        let module = host();
        numeric_structure!(
            &module,
            "SmudgyTextFrame",
            Uniforms,
            [
                origin,
                resolution,
                surface,
                texture_size,
                text_size,
                scale,
                outset,
                time,
                progress,
                seed,
                linearize,
                background,
                effect_scale,
                duration,
                envelope,
                replaces_text,
                paint_offset,
                capture_offset,
            ]
        );
    }

    #[test]
    fn cpu_glyph_geometry_and_array_stride_match_wgsl() {
        let module = host();
        numeric_structure!(
            &module,
            "SmudgyGlyph",
            GlyphGeometry,
            [advance, ink, cluster]
        );
        let fields = structure(&module, "SmudgyGlyphTable", size_of::<GlyphTable>());
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].name.as_deref(), Some("info"));
        assert_eq!(fields[0].offset as usize, offset_of!(GlyphTable, info));
        numeric(&module, &fields[0], &[0_u32; 4]);
        assert_eq!(fields[1].name.as_deref(), Some("items"));
        assert_eq!(fields[1].offset as usize, offset_of!(GlyphTable, items));
        let TypeInner::Array { base, size, stride } = module.types[fields[1].ty].inner else {
            panic!("glyph items must be a fixed-size array");
        };
        assert_eq!(module.types[base].name.as_deref(), Some("SmudgyGlyph"));
        assert_eq!(stride as usize, size_of::<GlyphGeometry>());
        let naga::ArraySize::Constant(count) = size else {
            panic!("glyph items must have a constant count");
        };
        assert_eq!(count.get() as usize, MAX_GLYPHS);
    }
}

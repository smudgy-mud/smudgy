//! Validated WGSL and reflected parameters, independent of the GPU renderer.
//!
//! Imports validate source before allocating a native handle. The shared host
//! declarations define coordinates, sampled glyphs, geometry and filtered noise;
//! adapters supply either a full-surface pass or bounded instanced glyph quads.
//! Author source remains first so diagnostics refer to the imported file's lines.
//! GPU pipeline creation and capture caching belong to `smudgy_ui_shared`.
use serde_json::Value;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

static NEXT_SHADER: AtomicU64 = AtomicU64::new(1);
/// Maximum UTF-8 source size accepted for one imported WGSL module.
pub const MAX_SHADER_SOURCE_BYTES: usize = 64 * 1024;
/// Bound diagnostic labels independently of source and renderer resources.
pub const MAX_SHADER_LABEL_BYTES: usize = 1024;
/// Maximum reflected parameter buffer size, including WGSL alignment padding.
pub const MAX_UNIFORM_BYTES: usize = 1024;
/// Maximum instanced quads per shaped glyph, separate from capture admission.
pub const MAX_FRAGMENTS_PER_GLYPH: u32 = 64;
/// Shared host declarations and sampling helpers, without entry points.
pub const COMMON: &str = include_str!("text_shader_common.wgsl");
/// Complete full-surface adapter, appended after the author's source.
pub const HOST: &str = concat!(
    include_str!("text_shader_common.wgsl"),
    "\n",
    include_str!("text_shader.wgsl")
);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub offset: usize,
    pub lanes: usize,
    pub kind: naga::ScalarKind,
}

/// Validated source and reflection. Renderer-specific pipelines are cached separately.
#[derive(Debug)]
pub struct Shader {
    pub id: u64,
    pub label: Arc<str>,
    pub source: Arc<str>,
    pub fields: Vec<Field>,
    pub uniform_size: usize,
    /// Zero uses the surface shader; otherwise bounded instanced quads per shaped glyph.
    pub fragments_per_glyph: u32,
}
impl PartialEq for Shader {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for Shader {}

impl Shader {
    /// Validate the complete renderer ABI before a script receives a handle.
    pub fn compile(label: &str, source: &str) -> Result<Arc<Self>, String> {
        if label.len() > MAX_SHADER_LABEL_BYTES {
            return Err("WGSL label exceeds 1 KiB".into());
        }
        if source.len() > MAX_SHADER_SOURCE_BYTES {
            return Err("WGSL source exceeds 64 KiB".into());
        }
        // Author source comes first so reported lines match the imported file.
        let declarations = format!("{source}\n{COMMON}");
        let declarations = naga::front::wgsl::parse_str(&declarations)
            .map_err(|e| format!("{label}: {}", e.emit_to_string(&declarations)))?;
        let instanced = declarations
            .functions
            .iter()
            .any(|(_, f)| f.name.as_deref() == Some("textVertex"));
        let fragments_per_glyph = if instanced {
            let constant = declarations
                .constants
                .iter()
                .find(|(_, c)| c.name.as_deref() == Some("TEXT_FRAGMENTS"))
                .ok_or_else(|| {
                    format!("{label}: textVertex requires const TEXT_FRAGMENTS: u32 = 1u..{MAX_FRAGMENTS_PER_GLYPH}u")
                })?
                .1;
            match declarations.global_expressions[constant.init] {
                naga::Expression::Literal(naga::Literal::U32(count))
                    if (1..=MAX_FRAGMENTS_PER_GLYPH).contains(&count) =>
                {
                    count
                }
                _ => {
                    return Err(format!(
                        "{label}: TEXT_FRAGMENTS must be a constant u32 between 1 and {MAX_FRAGMENTS_PER_GLYPH}"
                    ));
                }
            }
        } else {
            0
        };
        let source = if instanced {
            format!(
                "{source}\n{COMMON}\n{}",
                include_str!("text_shader_fragments.wgsl")
            )
        } else {
            format!("{source}\n{HOST}")
        };
        let module = naga::front::wgsl::parse_str(&source)
            .map_err(|e| format!("{label}: {}", e.emit_to_string(&source)))?;
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .map_err(|e| format!("{label}: {}", e.emit_to_string(&source)))?;
        if module.entry_points.len() != 2
            || module
                .entry_points
                .iter()
                .any(|e| !["smudgy_vs", "smudgy_fs"].contains(&e.name.as_str()))
        {
            return Err(format!(
                "{label}: supply effect(position: vec2f) -> vec4f, without entry points"
            ));
        }
        if !module.overrides.is_empty() {
            return Err(format!(
                "{label}: pipeline overrides are unsupported; use uniforms"
            ));
        }
        let mut fields = Vec::new();
        let mut parameters_seen = false;
        let mut uniform_size = 16;
        for (_, global) in module.global_variables.iter() {
            let Some(binding) = &global.binding else {
                continue;
            };
            if binding.group == 0
                && binding.binding <= 5
                && global.name.as_deref()
                    == Some(
                        [
                            "text",
                            "smudgy_glyphs",
                            "smudgy_sampler",
                            "smudgy_geometry",
                            "smudgy_noise",
                            "smudgy_noise_sampler",
                        ][binding.binding as usize],
                    )
            {
                continue;
            }
            if binding.group != 1
                || binding.binding != 0
                || global.space != naga::AddressSpace::Uniform
            {
                return Err(format!(
                    "{label}: custom resources must be one uniform struct at @group(1) @binding(0)"
                ));
            }
            if parameters_seen {
                return Err(format!("{label}: only one parameter binding is supported"));
            }
            parameters_seen = true;
            let naga::TypeInner::Struct { members, span } = &module.types[global.ty].inner else {
                return Err(format!(
                    "{label}: shader parameters must be a uniform struct"
                ));
            };
            uniform_size = (*span as usize).max(16).next_multiple_of(16);
            if uniform_size > MAX_UNIFORM_BYTES {
                return Err(format!(
                    "{label}: uniforms exceed {MAX_UNIFORM_BYTES} bytes"
                ));
            }
            for member in members {
                let (scalar, lanes) = match module.types[member.ty].inner {
                    naga::TypeInner::Scalar(scalar) => (scalar, 1),
                    naga::TypeInner::Vector { size, scalar } => (scalar, size as usize),
                    _ => {
                        return Err(format!(
                            "{label}: uniforms support f32/i32/u32 scalars and vectors only"
                        ));
                    }
                };
                if scalar.width != 4
                    || !matches!(
                        scalar.kind,
                        naga::ScalarKind::Float | naga::ScalarKind::Sint | naga::ScalarKind::Uint
                    )
                {
                    return Err(format!(
                        "{label}: uniforms support 32-bit numeric fields only"
                    ));
                }
                fields.push(Field {
                    name: member.name.clone().unwrap(),
                    offset: member.offset as usize,
                    lanes,
                    kind: scalar.kind,
                });
            }
        }
        Ok(Arc::new(Self {
            id: NEXT_SHADER.fetch_add(1, Ordering::Relaxed),
            label: label.into(),
            source: source.into(),
            fields,
            uniform_size,
            fragments_per_glyph,
        }))
    }

    /// Owned source/reflection payload for the importing isolate's byte budget.
    /// Shared references and GPU pipelines are accounted for by their own owners.
    pub fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.label.len()
            + self.source.len()
            + self.fields.capacity() * std::mem::size_of::<Field>()
            + self
                .fields
                .iter()
                .map(|field| field.name.capacity())
                .sum::<usize>()
    }

    /// Pack by reflected WGSL offsets, including vec3 padding. Reject typos and NaN.
    pub fn uniforms(&self, values: &serde_json::Map<String, Value>) -> Result<Arc<[u8]>, String> {
        if let Some(key) = values
            .keys()
            .find(|key| !self.fields.iter().any(|f| &f.name == *key))
        {
            return Err(format!("unknown shader uniform {key}"));
        }
        let mut bytes = vec![0; self.uniform_size];
        for field in &self.fields {
            let value = values
                .get(&field.name)
                .ok_or_else(|| format!("missing shader uniform {}", field.name))?;
            let values: Vec<&Value> = if field.lanes == 1 {
                vec![value]
            } else {
                let array = value
                    .as_array()
                    .filter(|v| v.len() == field.lanes)
                    .ok_or_else(|| format!("{} requires {} numbers", field.name, field.lanes))?;
                array.iter().collect()
            };
            for (i, value) in values.into_iter().enumerate() {
                let invalid = || format!("invalid {:?} shader uniform {}", field.kind, field.name);
                let packed = match field.kind {
                    naga::ScalarKind::Float => {
                        let f = value.as_f64().ok_or_else(invalid)? as f32;
                        if !f.is_finite() {
                            return Err(invalid());
                        }
                        f.to_le_bytes()
                    }
                    naga::ScalarKind::Sint => {
                        let value = value.as_f64().ok_or_else(invalid)?;
                        if !value.is_finite()
                            || value.fract() != 0.0
                            || value < f64::from(i32::MIN)
                            || value > f64::from(i32::MAX)
                        {
                            return Err(invalid());
                        }
                        (value as i32).to_le_bytes()
                    }
                    naga::ScalarKind::Uint => {
                        let value = value.as_f64().ok_or_else(invalid)?;
                        if !value.is_finite()
                            || value.fract() != 0.0
                            || value < 0.0
                            || value > f64::from(u32::MAX)
                        {
                            return Err(invalid());
                        }
                        (value as u32).to_le_bytes()
                    }
                    _ => unreachable!(),
                };
                let offset = field.offset + i * 4;
                bytes[offset..offset + 4].copy_from_slice(&packed);
            }
        }
        Ok(bytes.into())
    }
}

/// Validated geometric effect scale, stored exactly while retaining model equality.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectScale(u32);
impl Default for EffectScale {
    fn default() -> Self {
        Self(1.0_f32.to_bits())
    }
}
impl EffectScale {
    pub fn new(value: f32) -> Result<Self, String> {
        if !value.is_finite() || !(0.25..=4.0).contains(&value) {
            return Err("TextEffect scale must be between 0.25 and 4".into());
        }
        Ok(Self(value.to_bits()))
    }
    pub fn get(self) -> f32 {
        f32::from_bits(self.0)
    }
}

/// Requested glyph-capture resolution multiplier; the renderer still enforces its texture budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureScale(u32);
impl Default for CaptureScale {
    fn default() -> Self {
        Self(1.0_f32.to_bits())
    }
}
impl CaptureScale {
    pub fn new(value: f32) -> Result<Self, String> {
        if !value.is_finite() || !(1.0..=8.0).contains(&value) {
            return Err("TextEffect captureScale must be between 1 and 8".into());
        }
        Ok(Self(value.to_bits()))
    }
    pub fn get(self) -> f32 {
        f32::from_bits(self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShaderEffect {
    /// Paint across the containing viewport while the text anchor is visible.
    pub pane: bool,
    pub scale: EffectScale,
    pub capture_scale: CaptureScale,
    pub fade_in_ms: u32,
    pub fade_out_ms: u32,
    pub shader: Arc<Shader>,
    pub uniforms: Arc<[u8]>,
    /// Replace native foreground glyphs; otherwise paint behind readable text.
    pub replace: bool,
    /// Keep the final shader output after a finite animation stops.
    pub hold: bool,
    pub animated: bool,
}

impl ShaderEffect {
    /// Smooth attack/release; overlapping fades are shortened proportionally.
    pub fn envelope(&self, elapsed: std::time::Duration, duration_ms: u32) -> f32 {
        let seconds = |ms| std::time::Duration::from_millis(u64::from(ms)).as_secs_f32();
        let duration = seconds(duration_ms);
        let mut attack = seconds(self.fade_in_ms);
        let mut release = if duration_ms == 0 {
            0.0
        } else {
            seconds(self.fade_out_ms)
        };
        if duration > 0.0 && attack + release > duration {
            let ratio = duration / (attack + release);
            attack *= ratio;
            release *= ratio;
        }
        let smooth = |v: f32| {
            let v = v.clamp(0.0, 1.0);
            v * v * (3.0 - 2.0 * v)
        };
        let t = elapsed.as_secs_f32();
        let fade_in = if attack > 0.0 {
            smooth(t / attack)
        } else {
            1.0
        };
        let fade_out = if release > 0.0 {
            smooth((duration - t) / release)
        } else {
            1.0
        };
        fade_in.min(fade_out)
    }
}

#[cfg(test)]
mod tests;

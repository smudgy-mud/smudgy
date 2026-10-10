//! Per-isolate validated-source budget, separate from GPU and capture admission.
use smudgy_session_model::text_shader::Shader;
use std::sync::{Arc, Weak};

const MAX_SHADERS: usize = 4096;
const MAX_SHADER_BYTES: usize = 64 * 1024 * 1024;

#[derive(Default)]
pub(crate) struct ShaderBudget(Vec<(Weak<Shader>, usize)>);

impl ShaderBudget {
    pub(crate) fn compile(&mut self, label: &str, source: &str) -> Result<Arc<Shader>, String> {
        self.0.retain(|(shader, _)| shader.strong_count() > 0);
        if self.0.len() >= MAX_SHADERS {
            return Err(format!(
                "an isolate can retain at most {MAX_SHADERS} text shaders"
            ));
        }
        let shader = Shader::compile(label, source)?;
        let bytes = shader.retained_bytes();
        if bytes + self.0.iter().map(|(_, bytes)| bytes).sum::<usize>() > MAX_SHADER_BYTES {
            return Err("retained text shader source and reflection exceed 64 MiB".into());
        }
        self.0.push((Arc::downgrade(&shader), bytes));
        Ok(shader)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COPY: &str = "fn effect(p: vec2f) -> vec4f { return sampleText(p); }";

    #[test]
    fn a_catalogue_can_exceed_the_old_64_shader_limit() {
        let mut cache = ShaderBudget::default();
        let shaders: Vec<_> = (0..128)
            .map(|_| cache.compile("copy", COPY).unwrap())
            .collect();
        assert_eq!(cache.0.len(), shaders.len());
        drop(shaders);
        let _new = cache.compile("new generation", COPY).unwrap();
        assert_eq!(cache.0.len(), 1);
    }

    #[test]
    fn count_limit_denies_before_parsing_and_reclaims_dropped_handles() {
        let mut cache = ShaderBudget::default();
        let shader = cache.compile("retained", COPY).unwrap();
        // Each weak entry represents a live handle. Avoid thousands of duplicate
        // Naga validations when exercising the admission boundary itself.
        cache.0.resize(
            MAX_SHADERS,
            (Arc::downgrade(&shader), shader.retained_bytes()),
        );
        assert!(
            cache
                .compile("overflow", "invalid")
                .unwrap_err()
                .contains("4096")
        );
        drop(shader);
        let _new = cache.compile("reclaimed", COPY).unwrap();
        assert_eq!(cache.0.len(), 1);
    }

    #[test]
    fn byte_limit_is_inclusive_and_failed_validation_does_not_consume_budget() {
        let mut cache = ShaderBudget::default();
        let shader = cache.compile("existing", COPY).unwrap();
        let bytes = shader.retained_bytes();
        cache.0[0].1 = MAX_SHADER_BYTES - bytes;
        let exact = cache.compile("existing", COPY).unwrap();
        assert_eq!(cache.0.len(), 2);
        assert!(
            cache
                .compile("overflow", COPY)
                .unwrap_err()
                .contains("64 MiB")
        );
        assert!(cache.compile("bad", "invalid").is_err());
        assert_eq!(cache.0.len(), 2);
        drop(exact);
        let _reclaimed = cache.compile("existing", COPY).unwrap();
        assert_eq!(cache.0.len(), 2);
    }
}

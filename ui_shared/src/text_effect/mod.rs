//! GPU effects over native shaped text, shared by terminal and ordinary widgets.
//!
//! The session model validates WGSL, parameters and playback options. This module
//! owns visible-input admission and the renderer adapter; it has no scripting
//! runtime dependency. Native paragraphs remain the source for layout, selection,
//! links and text projection even when an effect replaces their foreground ink.
//!
//! Each wrapped fragment has an [`Input`] lease. The GPU layer captures its styled
//! glyphs and per-occurrence geometry, while renderer-local caches share matching
//! immutable pixels. Retirement releases admission without tying idle cached pixels
//! to scrollback or retaining script objects. Compiled programs live in iced's
//! engine-owned custom storage; the global prewarm registry holds only weak references.
//!
//! Animation updates frame uniforms and requests native redraws. It does not run
//! JavaScript, reshape text or upload captured pixels every frame. If a program or
//! input is not ready, native text stays visible. Rendering effects requires wgpu;
//! software renderers continue to display ordinary text.
mod gpu;
#[cfg(any(test, feature = "profiling"))]
use crate::profiling::{CaptureCounters as Counters, CaptureStats as Stats};
use iced::{Rectangle, advanced::graphics::text};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
static INPUTS: AtomicUsize = AtomicUsize::new(0);
/// Process-wide visible text captures. Wrapped fragments consume separate leases.
const MAX_INPUTS: usize = 64;
#[derive(Debug)]
struct InputLease(AtomicBool);
impl InputLease {
    fn release(&self) {
        if self.0.swap(false, Ordering::Relaxed) {
            INPUTS.fetch_sub(1, Ordering::Relaxed);
        }
    }
}
impl Drop for InputLease {
    fn drop(&mut self) {
        self.release();
    }
}
use smudgy_session_model::inline_content::InlineDecoration;
#[derive(Debug, Clone)]
pub struct Input {
    paragraph: text::Paragraph,
    region: Rectangle,
    _lease: Arc<InputLease>,
    identity: Arc<()>,
    #[cfg(any(test, feature = "profiling"))]
    counters: Arc<Counters>,
    pending: Arc<AtomicBool>,
}
impl Input {
    /// Retain counters independently of the input's admission/lifetime.
    #[cfg(any(test, feature = "profiling"))]
    pub fn counters(&self) -> Arc<Counters> {
        self.counters.clone()
    }
    #[cfg(any(test, feature = "profiling"))]
    pub fn stats(&self) -> Stats {
        self.counters.stats()
    }
    pub fn admitted(&self) -> bool {
        self._lease.0.load(Ordering::Relaxed)
    }
    pub fn can_admit(&self, fragments: usize) -> bool {
        self.admitted() || fragments <= MAX_INPUTS.saturating_sub(INPUTS.load(Ordering::Relaxed))
    }
    pub fn admit(&self) -> bool {
        if self.admitted() {
            return true;
        }
        // Called on the renderer thread, before hiding native glyphs. Retry each
        // visible frame so scrolling and resize can recover from a full budget.
        if INPUTS
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                (count < MAX_INPUTS).then_some(count + 1)
            })
            .is_err()
        {
            return false;
        }
        if self._lease.0.swap(true, Ordering::Relaxed) {
            INPUTS.fetch_sub(1, Ordering::Relaxed);
        }
        true
    }
    pub fn pending(&self) -> bool {
        self.pending.load(Ordering::Relaxed)
    }
    pub fn retire(&self) {
        self.pending.store(false, Ordering::Relaxed);
        self._lease.release();
    }
    pub fn from_paragraph<P: 'static>(paragraph: &P, region: Rectangle) -> Option<Self> {
        let paragraph = (paragraph as &dyn std::any::Any).downcast_ref::<text::Paragraph>()?;
        Some(Self {
            paragraph: paragraph.clone(),
            region,
            _lease: Arc::new(InputLease(AtomicBool::new(false))),
            identity: Arc::default(),
            #[cfg(any(test, feature = "profiling"))]
            counters: Arc::default(),
            pending: Arc::new(AtomicBool::new(false)),
        })
    }
}
pub fn supported(renderer: &dyn std::any::Any) -> bool {
    renderer.is::<iced_wgpu::Renderer>()
        || matches!(
            renderer.downcast_ref::<iced::Renderer>(),
            Some(iced::Renderer::Primary(_))
        )
}
pub(crate) fn ready(
    renderer: &dyn std::any::Any,
    input: &Input,
    effect: &Arc<smudgy_session_model::text_shader::ShaderEffect>,
) -> bool {
    gpu::ready(renderer, input, effect)
}
pub fn draw(
    renderer: &mut dyn std::any::Any,
    input: &Input,
    bounds: Rectangle,
    viewport: Rectangle,
    effect: &InlineDecoration,
    now: iced::time::Instant,
) -> bool {
    if viewport.width <= 0.0 || viewport.height <= 0.0 {
        input.retire();
        return false;
    }
    let shader = &effect.effect.shader;
    if effect.elapsed(now).is_none() || (shader.pane && bounds.intersection(&viewport).is_none()) {
        input.retire();
        return false;
    }
    gpu::draw(renderer, input, bounds, viewport, effect, shader, now)
}

/// Register a renderer before scripts import WGSL, allowing compilation off the UI thread.
pub fn register_renderer(renderer: &dyn std::any::Any) {
    let _ = gpu::register(renderer);
}
/// Prewarm the active GPU. Headless/no-window imports still validate on the CPU;
/// their pipeline is prepared asynchronously when first used.
///
/// # Errors
/// Returns a labelled driver/validation error if an active engine rejects the shader.
pub fn prewarm(shader: &Arc<smudgy_session_model::text_shader::Shader>) -> Result<(), String> {
    gpu::prewarm(shader)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod alignment_tests;

#[cfg(test)]
#[path = "../../../tests/text-effects/gpu.rs"]
mod catalogue_tests;

#[cfg(any(test, feature = "profiling"))]
pub(crate) fn admitted_inputs() -> usize {
    INPUTS.load(Ordering::Relaxed)
}

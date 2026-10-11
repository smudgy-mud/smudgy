//! Bounded shader painter shared by terminal and ordinary text hosts.
use iced::Rectangle;
use iced::time::Instant;
use smudgy_session_model::inline_content::InlineDecoration;

pub fn paint_bounds(bounds: Rectangle, effect: &InlineDecoration) -> Rectangle {
    if effect.effect.shader.pane {
        // Pane effects stop when their anchor leaves the viewport.
        bounds
    } else {
        bounds.expand(f32::from(effect.effect.outset))
    }
}

/// Returns whether this instance needs another frame. No script executes here.
pub fn draw_with_fuel(
    renderer: &mut (impl iced::advanced::Renderer + 'static),
    effect: &InlineDecoration,
    bounds: Rectangle,
    viewport: Rectangle,
    now: Instant,
    fuel: Option<&crate::text_effect::Input>,
) -> bool {
    let Some(input) = fuel else {
        return false;
    };
    if effect.elapsed(now).is_none() {
        input.retire();
        return false;
    }
    if !crate::text_effect::supported(renderer)
        || paint_bounds(bounds, effect)
            .intersection(&viewport)
            .is_none()
    {
        return false;
    }
    renderer.with_layer(viewport, |renderer| {
        crate::text_effect::draw(renderer, input, bounds, viewport, effect, now);
    });
    effect.animated(now)
}

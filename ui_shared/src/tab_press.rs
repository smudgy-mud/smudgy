//! The tab's press surface: the widget that owns click-vs-drag
//! disambiguation for one pane tab.
//!
//! A left press on the surface arms a drag candidate at its true press
//! point. Motion within [`DRAG_DEADBAND`] keeps the gesture a click — the
//! release selects the tab; crossing the deadband makes the drag live, and
//! from there the daemon's drag controller owns cursor tracking, targeting,
//! and the terminal operation. The widget's own release event is a fast
//! path: the daemon's drag-gated subscription maps the raw button release as
//! the authoritative terminal, so a release this widget never sees (subtree
//! rebuilt mid-drag, event captured elsewhere) still ends the drag.
//!
//! Two contracts are deliberate here:
//!
//! - A release carries an *optional* cursor point. An unavailable cursor is
//!   reported as `None` — a cancel signal — never approximated by a
//!   fabricated origin.
//! - Modifier state is handed in at view time from window-level tracking.
//!   The widget keeps no shadow copy from `ModifiersChanged`, which a
//!   subtree rebuild would silently reset.
//!
//! Embedded tab controls (connect/disconnect, close, visibility) are sibling
//! elements outside this surface; a press on them acts on the control and
//! can never start a drag.

use iced::advanced::renderer::Quad;
use iced::advanced::widget::{Operation, tree};
use iced::advanced::{Renderer as _, Widget, layout, overlay};
use iced::{Event as IcedEvent, Length, Point, Rectangle, Size, Vector, keyboard, mouse, window};

use crate::pane_drag::DRAG_DEADBAND;
use smudgy_theme::{Element, Theme};

/// A press-surface state transition, published as it happens. Points are in
/// the coordinate space the surface is updated in — its host scrollable's
/// content space, which sits `scroll offset` to the right of window space
/// when the strip is scrolled. The strip boundary grounds them to window
/// space ([`crate::components::tab_strip::ground_to_window`]) before they
/// reach the daemon, whose cursor tracking is window-space.
#[derive(Debug, Clone, Copy)]
pub enum Event {
    /// Left press with no modifier: a drag candidate armed at its true press
    /// point. The daemon refreshes window geometry ahead of a possible drag.
    Pressed { point: Point },
    /// Left press with a modifier held: reserved control behavior — never a
    /// drag, never a drag-machinery selection.
    ModifiedPress,
    /// Released within the deadband: a plain click — select the tab.
    Click,
    /// The press crossed the deadband: the drag is live from here until a
    /// terminal event.
    DragStarted { press: Point, point: Point },
    /// Released past the deadband. `None` when the cursor position was
    /// unavailable at release — the daemon treats that as a cancel.
    DragReleased { point: Option<Point> },
    /// The source window lost focus with the button down — the narrow proxy
    /// for a true OS capture loss (alt-tab, Win+L).
    CaptureLost { dragging: bool },
}

/// Where the surface is in its press/drag lifecycle.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
enum Phase {
    #[default]
    Idle,
    /// Pressed, still within the deadband of `press`.
    Pressed { press: Point },
    /// Past the deadband: the drag is live.
    Dragging,
}

#[derive(Debug, Default)]
struct State {
    phase: Phase,
}

/// The press surface wrapping one tab's label content. `drag_live` mirrors
/// the daemon's drag record: when the daemon ends a drag externally (Escape,
/// cancel, terminal handled elsewhere) the flag drops and a surface still in
/// `Dragging` stands down on the next diff instead of waiting for a release
/// that may never arrive. A press below the deadband is deliberately *not*
/// reset by the flag: Escape while pressed leaves the press running.
pub struct TabPress<'a, Message> {
    content: Element<'a, Message>,
    drag_live: bool,
    modifiers: keyboard::Modifiers,
    /// Diagnostic identity (the tab's stable id) for the fresh-subtree
    /// tripwire log.
    tag: u64,
    on_event: Box<dyn Fn(Event) -> Message + 'a>,
}

impl<'a, Message> TabPress<'a, Message> {
    pub fn new(
        content: impl Into<Element<'a, Message>>,
        drag_live: bool,
        modifiers: keyboard::Modifiers,
        tag: u64,
        on_event: impl Fn(Event) -> Message + 'a,
    ) -> Self {
        Self {
            content: content.into(),
            drag_live,
            modifiers,
            tag,
            on_event: Box::new(on_event),
        }
    }
}

impl<Message> Widget<Message, Theme, iced::Renderer> for TabPress<'_, Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        // A fresh-state construction after startup means the widget tree
        // REBUILT this surface (a diff mismatch above it): any in-flight
        // press/drag phase is silently lost, which downstream looks like
        // "the gesture never continued". The measured failure mode this
        // tripwire exists to catch.
        log::info!(
            "[pane-drag] tab press state created for tab {} (fresh subtree)",
            self.tag
        );
        tree::State::new(State::default())
    }

    fn children(&self) -> Vec<tree::Tree> {
        vec![tree::Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut tree::Tree) {
        let state = tree.state.downcast_mut::<State>();
        if state.phase == Phase::Dragging && !self.drag_live {
            // External termination (daemon-side cancel or authoritative
            // release): stand down so the next release cannot report a
            // phantom drag end.
            state.phase = Phase::Idle;
        }
        tree.diff_children(std::slice::from_ref(&self.content));
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut tree::Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut tree::Tree,
        layout: layout::Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut tree::Tree,
        event: &IcedEvent,
        layout: layout::Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn iced::advanced::Clipboard,
        shell: &mut iced::advanced::Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );

        let state = tree.state.downcast_mut::<State>();
        match event {
            IcedEvent::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let Some(point) = cursor.position() else {
                    return;
                };
                if !cursor.is_over(layout.bounds()) {
                    return;
                }
                if self.modifiers.control()
                    || self.modifiers.alt()
                    || self.modifiers.logo()
                    || self.modifiers.shift()
                {
                    shell.publish((self.on_event)(Event::ModifiedPress));
                } else {
                    state.phase = Phase::Pressed { press: point };
                    shell.publish((self.on_event)(Event::Pressed { point }));
                }
                shell.capture_event();
            }
            IcedEvent::Mouse(mouse::Event::CursorMoved { .. }) => {
                // The deadband is measured with `cursor`, not the raw event
                // position: a host scrollable translates the cursor it hands
                // children into its content space (where `press` lives) but
                // passes the event through in window space — mixing the two
                // would count the scroll offset as travel and promote every
                // click on a scrolled strip. An unavailable cursor (outside
                // the scrollable's viewport) promotes nothing here; the
                // daemon's window-space tracking owns that crossing.
                if let Phase::Pressed { press } = state.phase
                    && let Some(position) = cursor.position()
                    && position.distance(press) > DRAG_DEADBAND
                {
                    state.phase = Phase::Dragging;
                    shell.publish((self.on_event)(Event::DragStarted {
                        press,
                        point: position,
                    }));
                    shell.capture_event();
                }
                // While Dragging, motion belongs to the daemon tracker.
            }
            IcedEvent::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                match state.phase {
                    Phase::Pressed { .. } => {
                        state.phase = Phase::Idle;
                        shell.publish((self.on_event)(Event::Click));
                        shell.capture_event();
                    }
                    Phase::Dragging => {
                        state.phase = Phase::Idle;
                        shell.publish((self.on_event)(Event::DragReleased {
                            point: cursor.position(),
                        }));
                        shell.capture_event();
                    }
                    Phase::Idle => {}
                }
            }
            IcedEvent::Window(window::Event::Unfocused) => match state.phase {
                Phase::Pressed { .. } => {
                    state.phase = Phase::Idle;
                    shell.publish((self.on_event)(Event::CaptureLost { dragging: false }));
                }
                Phase::Dragging => {
                    state.phase = Phase::Idle;
                    shell.publish((self.on_event)(Event::CaptureLost { dragging: true }));
                }
                Phase::Idle => {}
            },
            _ => {}
        }
    }

    fn draw(
        &self,
        tree: &tree::Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &iced::advanced::renderer::Style,
        layout: layout::Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        // Hover feedback: a translucent wash under the label while the
        // pointer rests on the surface and no drag is in flight. Draw-only —
        // hit-testing, event routing, and the press lifecycle are untouched.
        if !self.drag_live && cursor.is_over(layout.bounds()) {
            renderer.fill_quad(
                Quad {
                    bounds: layout.bounds(),
                    // Follows the hosting tab's rounded leading corner; the
                    // label surface is the tab's leftmost element.
                    border: iced::Border {
                        radius: iced::border::Radius {
                            top_left: 6.0,
                            top_right: 0.0,
                            bottom_right: 0.0,
                            bottom_left: 0.0,
                        },
                        ..Default::default()
                    },
                    ..Quad::default()
                },
                theme.styles.tabs.hover_wash,
            );
        }
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &tree::Tree,
        layout: layout::Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        let state = tree.state.downcast_ref::<State>();
        match state.phase {
            Phase::Dragging => mouse::Interaction::Grabbing,
            Phase::Pressed { .. } => mouse::Interaction::Grab,
            Phase::Idle => self.content.as_widget().mouse_interaction(
                &tree.children[0],
                layout,
                cursor,
                viewport,
                renderer,
            ),
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut tree::Tree,
        layout: layout::Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, iced::Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message> From<TabPress<'a, Message>> for Element<'a, Message>
where
    Message: 'a,
{
    fn from(press: TabPress<'a, Message>) -> Self {
        Element::new(press)
    }
}

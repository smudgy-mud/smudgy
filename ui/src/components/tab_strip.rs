//! The tab strip of one pane group: one tab per member, rendered from plain
//! descriptors behind a component boundary.
//!
//! The strip is data-driven — callers describe tabs, they don't hand in
//! widgets — so richer tab content (script-customized tabs) can extend the
//! descriptor without changing the hosting window. Every interactive control
//! is a discrete element beside the tab's label surface, never layered over
//! it: the label surface is the tab's drag region (a [`TabPress`] owning the
//! click-vs-drag deadband), and the embedded controls (connect/disconnect,
//! close, visibility) are sibling elements a press can only ever operate —
//! never drag.
//!
//! The strip also mirrors its geometry for drag hit-testing: the hosting
//! window hands in draw-time callbacks recording the strip band and each
//! tab's span, plus receives scroll-offset changes, so the drag controller
//! can classify header drops (insertion slots included) without walking the
//! widget tree per cursor move.
//!
//! Overflow: the strip scrolls horizontally, and [`reveal`] keeps a selected
//! tab in view. The scroll container is an implementation detail of this
//! module: callers see only descriptors and events, so the overflow strategy
//! is swappable without touching them.
//!
//! [`TabPress`]: crate::widgets::tab_press::TabPress

use iced::advanced::widget::{Id, Operation, operation};
use iced::widget::{container, row, scrollable, text, tooltip};
use iced::{Alignment, Border, Length, Padding, Point, Rectangle, Task, Vector, keyboard};

use crate::pane_groups::TabId;
use crate::session_store::title_bar_icon_button;
use crate::theme::Element as ThemedElement;
use crate::widgets::{bounds_probe::BoundsProbe, tab_press};
use crate::{assets, i18n, theme};

/// A user action on the strip, mapped by the hosting window onto its own
/// message type.
#[derive(Debug, Clone, Copy)]
pub enum Event {
    /// A tab's label surface was clicked (released below the drag deadband):
    /// select it (and activate its session).
    Select(TabId),
    /// A press-surface drag transition on one tab (press, deadband crossing,
    /// release, capture loss). The hosting window forwards these to the
    /// daemon's drag controller.
    Drag(TabId, tab_press::Event),
    /// The strip scrolled; carries the new absolute x offset for the
    /// window's geometry mirror.
    Scrolled(f32),
    /// The main tab's connection control while disconnected.
    Connect(TabId),
    /// The main tab's connection control while connected.
    Disconnect(TabId),
    /// The main tab's close control: close the whole session.
    CloseSession(TabId),
    /// The tab's visibility eye.
    ToggleVisibility(TabId),
}

/// Window-supplied context for one strip: drag-view state handed down from
/// the daemon (no per-window drag flags) and the draw-time geometry mirrors.
/// The callbacks are owned (they ride inside the built elements and must
/// outlive the view pass that created them).
pub struct StripContext<'a> {
    /// Whether a pane drag is live anywhere in the app. Resets a stale
    /// press surface and keeps the strip's drag affordances honest.
    pub drag_live: bool,
    /// Window-level keyboard modifier state, passed at view time — the
    /// press surface keeps no shadow copy.
    pub modifiers: keyboard::Modifiers,
    /// Whether tabs carry their visibility eye. Rearrange mode only: a
    /// collapsed-toolbar strip lists exactly the tabs whose panes render,
    /// and a hide there would vanish the tab with no in-strip way back —
    /// the eye retreats to the expanded toolbar's strip, where hidden
    /// tabs stay listed (dimmed) beside it.
    pub visibility_eyes: bool,
    /// Records the strip's on-screen band (window-local).
    pub on_strip_bounds: Box<dyn Fn(Rectangle) + 'a>,
    /// Records one tab's span (strip content space; pair with the scroll
    /// offset from [`Event::Scrolled`]).
    pub on_tab_bounds: std::rc::Rc<dyn Fn(TabId, Rectangle) + 'a>,
}

/// Everything the strip needs to render one tab.
pub struct TabDescriptor {
    pub id: TabId,
    /// Display label: profile/server for a main tab, the pane name for a
    /// script tab.
    pub label: String,
    /// Main-session tab (carries connection + close controls); `false` for a
    /// script-pane tab.
    pub main: bool,
    /// The group's durable selection.
    pub selected: bool,
    /// The tab the group's body currently renders (the effective selection —
    /// differs from `selected` when the selected tab is hidden).
    pub rendered: bool,
    /// User-hidden (the eye): dimmed in the strip, body suppressed.
    pub hidden: bool,
    /// The tab's session is the window's active session.
    pub active_session: bool,
    /// The tab's session is connected.
    pub connected: bool,
    /// The tab's session has connected at least once (drives the
    /// Connect/Reconnect label).
    pub ever_connected: bool,
}

/// Keep-visible margin inside the strip viewport when revealing a tab.
const REVEAL_PADDING: f32 = 12.0;

/// Grounds a point published from inside the strip's scrollable into window
/// space. The scrollable hands its children a scroll-translated cursor, so a
/// press surface reports points in strip content space — `scroll_x` further
/// right than the window-space location the daemon's raw cursor tracking
/// sees. The hosting window applies this at the strip boundary using its
/// scroll-offset mirror (fresh at press time: `on_scroll` delivers every
/// offset change, and no scroll can land between the mirror's last update
/// and a press). Without the grounding, a deadband measured against tracked
/// window-space samples would count the scroll offset as travel and promote
/// every click on a scrolled strip into a drag.
pub fn ground_to_window(point: Point, scroll_x: f32) -> Point {
    Point::new(point.x - scroll_x, point.y)
}

/// The strip for one group. `strip_id` must be stable across rebuilds (it is
/// the scroll anchor [`reveal`] targets); `anchor` supplies each tab's
/// equally stable container id.
pub fn view<'a, M: Clone + 'static>(
    strip_id: Id,
    tabs: impl IntoIterator<Item = TabDescriptor>,
    anchor: impl Fn(TabId) -> Id,
    context: StripContext<'a>,
    on_event: impl Fn(Event) -> M + Clone + 'a,
) -> ThemedElement<'a, M> {
    let StripContext {
        drag_live,
        modifiers,
        visibility_eyes,
        on_strip_bounds,
        on_tab_bounds,
    } = context;
    let mut strip = row![].spacing(2).align_y(Alignment::End);
    for descriptor in tabs {
        let id = anchor(descriptor.id);
        strip = strip.push(tab_element(
            descriptor,
            id,
            drag_live,
            modifiers,
            visibility_eyes,
            on_tab_bounds.clone(),
            on_event.clone(),
        ));
    }
    let scroll_event = on_event.clone();
    BoundsProbe::new(
        scrollable(strip)
            .id(strip_id)
            .direction(scrollable::Direction::Horizontal(
                scrollable::Scrollbar::new()
                    .width(2)
                    .scroller_width(2)
                    .margin(0),
            ))
            .on_scroll(move |viewport| scroll_event(Event::Scrolled(viewport.absolute_offset().x)))
            .width(Length::Shrink)
            .height(Length::Shrink),
        on_strip_bounds,
    )
    .into()
}

/// One tab: a press-owning label surface beside its embedded controls,
/// inside one visual boundary.
fn tab_element<'a, M: Clone + 'static>(
    descriptor: TabDescriptor,
    anchor: Id,
    drag_live: bool,
    modifiers: keyboard::Modifiers,
    visibility_eyes: bool,
    on_tab_bounds: std::rc::Rc<dyn Fn(TabId, Rectangle) + 'a>,
    on_event: impl Fn(Event) -> M + Clone + 'a,
) -> ThemedElement<'a, M> {
    let TabDescriptor {
        id,
        label,
        main,
        selected,
        rendered,
        hidden,
        active_session,
        connected,
        ever_connected,
    } = descriptor;

    // Text emphasis mirrors the pre-group session tab: full strength on the
    // active session's rendered tab, dimmer otherwise; hidden tabs dim
    // further and a disconnected session shades toward the muted end. The
    // ladder scales the theme's base label color, so every theme keeps the
    // same state contrast.
    let mut alpha: f32 = match (rendered, active_session) {
        (true, true) => 1.0,
        (true, false) => 0.75,
        (false, _) => 0.5,
    };
    if hidden {
        alpha *= 0.5;
    }
    if !connected {
        alpha *= 0.85;
    }

    let (size, label_alpha) = if main {
        (13, alpha)
    } else {
        (11, alpha * 0.85)
    };
    let label_text: ThemedElement<'a, M> = text(label)
        .size(size)
        .style(move |theme: &crate::Theme| iced::widget::text::Style {
            color: Some(theme.styles.tabs.label.scale_alpha(label_alpha)),
        })
        .into();
    // The label surface is the tab's drag handle: a press surface that owns
    // the click-vs-drag deadband from the true press point. A sub-deadband
    // release selects; crossing the deadband hands the gesture to the drag
    // controller. A modified press is reserved control behavior — neither.
    let press_event = on_event.clone();
    let label_surface: ThemedElement<'a, M> = tab_press::TabPress::new(
        container(label_text).padding(Padding {
            top: if main { 5.0 } else { 4.0 },
            right: 6.0,
            bottom: if main { 5.0 } else { 4.0 },
            left: if main { 10.0 } else { 8.0 },
        }),
        drag_live,
        modifiers,
        id.as_u64(),
        move |event| match event {
            tab_press::Event::Click => press_event(Event::Select(id)),
            other => press_event(Event::Drag(id, other)),
        },
    )
    .into();

    let mut items = row![label_surface].spacing(2).align_y(Alignment::Center);

    if main {
        let (conn_label, conn_event) = if connected {
            (
                i18n::ts!("session-action-disconnect"),
                Event::Disconnect(id),
            )
        } else if ever_connected {
            (i18n::ts!("session-action-reconnect"), Event::Connect(id))
        } else {
            (i18n::ts!("session-action-connect"), Event::Connect(id))
        };
        items = items.push(
            iced::widget::button(text(conn_label).size(11))
                .style(theme::builtins::button::subtle)
                .padding([1, 8])
                .on_press(on_event(conn_event)),
        );
        items = items.push(labeled(
            title_bar_icon_button(
                assets::hero_icons::X_MARK.clone(),
                on_event(Event::CloseSession(id)),
            ),
            i18n::ts!("pane-tab-close"),
        ));
    }

    // The visibility eye rides rearrange mode: there the strip lists
    // hidden tabs dimmed beside their visible peers, and the eye at each
    // tab's trailing edge is the recovery path. A collapsed-toolbar strip
    // lists only rendering tabs and carries no eyes at all — a hide there
    // would vanish the tab with no in-strip way back.
    if visibility_eyes {
        items = items.push(labeled(
            title_bar_icon_button(
                if hidden {
                    assets::hero_icons::EYE_SLASH.clone()
                } else {
                    assets::hero_icons::EYE.clone()
                },
                on_event(Event::ToggleVisibility(id)),
            ),
            if hidden {
                i18n::ts!("pane-tab-show")
            } else {
                i18n::ts!("pane-tab-hide")
            },
        ));
    }

    let tab = container(items)
        .id(anchor)
        .padding(Padding {
            top: 0.0,
            right: 4.0,
            bottom: 0.0,
            left: 0.0,
        })
        .style(move |theme: &crate::Theme| {
            let tabs = &theme.styles.tabs;
            let surface = match (rendered, active_session) {
                (true, true) => tabs.surface_active,
                (true, false) => tabs.surface_rendered,
                (false, _) => tabs.surface_inactive,
            };
            container::Style {
                background: Some(surface.into()),
                border: Border {
                    radius: iced::border::Radius {
                        top_left: 6.0,
                        top_right: 6.0,
                        bottom_right: 0.0,
                        bottom_left: 0.0,
                    },
                    width: if selected && !rendered { 1.0 } else { 0.0 },
                    // A selected-but-hidden tab keeps a faint marker of its
                    // preferred status while a fallback renders.
                    color: tabs.selection_marker,
                },
                ..Default::default()
            }
        });
    BoundsProbe::new(tab, move |bounds| on_tab_bounds(id, bounds)).into()
}

/// Wrap an icon-only control with its accessible label (shown as a tooltip).
fn labeled<'a, M: Clone + 'static>(
    control: ThemedElement<'a, M>,
    label: &'static str,
) -> ThemedElement<'a, M> {
    tooltip(
        control,
        container(text(label).size(11))
            .padding([2, 6])
            .style(theme::builtins::container::tooltip),
        tooltip::Position::Bottom,
    )
    .into()
}

/// The horizontal offset that brings `target` (a tab's laid-out bounds, in
/// the strip content's un-scrolled coordinates anchored at `viewport`'s
/// origin) fully into the strip viewport with a keep-visible margin, given
/// the current scroll `translation_x`. `None` when the tab is already
/// visible (no scroll needed).
fn reveal_offset(viewport: Rectangle, translation_x: f32, target: Rectangle) -> Option<f32> {
    let left = target.x - viewport.x;
    let right = left + target.width;
    let desired = if left - REVEAL_PADDING < translation_x {
        left - REVEAL_PADDING
    } else if right + REVEAL_PADDING > translation_x + viewport.width {
        right + REVEAL_PADDING - viewport.width
    } else {
        translation_x
    };
    let desired = desired.max(0.0);
    ((desired - translation_x).abs() >= 0.5).then_some(desired)
}

/// Phase 1 of [`reveal`]: reads the strip viewport (and its current
/// translation) plus the target tab's laid-out bounds from the live widget
/// tree, then chains an [`ApplyReveal`] when a scroll is needed.
struct FindTab {
    strip: Id,
    tab: Id,
    viewport: Option<(Rectangle, Vector)>,
    target: Option<Rectangle>,
}

impl Operation<f32> for FindTab {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<f32>)) {
        operate(self);
    }

    fn scrollable(
        &mut self,
        id: Option<&Id>,
        bounds: Rectangle,
        _content_bounds: Rectangle,
        translation: Vector,
        _state: &mut dyn operation::Scrollable,
    ) {
        if id == Some(&self.strip) {
            self.viewport = Some((bounds, translation));
        }
    }

    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        if id == Some(&self.tab) {
            self.target = Some(bounds);
        }
    }

    fn finish(&self) -> operation::Outcome<f32> {
        let (Some((viewport, translation)), Some(target)) = (self.viewport, self.target) else {
            return operation::Outcome::None;
        };
        match reveal_offset(viewport, translation.x, target) {
            Some(desired) => operation::Outcome::Chain(Box::new(ApplyReveal {
                strip: self.strip.clone(),
                desired,
            })),
            None => operation::Outcome::None,
        }
    }
}

/// Phase 2 of [`reveal`]: applies the computed offset and emits it as the
/// task's output. A scroll applied through a widget operation never fires
/// the scrollable's `on_scroll`, so the emission is the only way the hosting
/// window's scroll mirror learns about a reveal — without it, every
/// attention-moving drop would shift the strip under an unmoved mirror and
/// later header drops would classify against displaced tab bands.
struct ApplyReveal {
    strip: Id,
    desired: f32,
}

impl Operation<f32> for ApplyReveal {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<f32>)) {
        operate(self);
    }

    fn scrollable(
        &mut self,
        id: Option<&Id>,
        _bounds: Rectangle,
        _content_bounds: Rectangle,
        _translation: Vector,
        state: &mut dyn operation::Scrollable,
    ) {
        if id == Some(&self.strip) {
            state.scroll_to(operation::scrollable::AbsoluteOffset {
                x: Some(self.desired),
                y: None,
            });
        }
    }

    fn finish(&self) -> operation::Outcome<f32> {
        operation::Outcome::Some(self.desired)
    }
}

/// A task that scrolls `strip` just far enough that the tab anchored at
/// `tab` is fully visible (with a small margin), producing the offset it
/// scrolled to. Geometry is read from the live widget tree; if either widget
/// hasn't laid out yet, or the tab is already visible, the task produces
/// nothing. The caller must route the produced offset into the same mirror
/// its `on_scroll` handler feeds — see [`ApplyReveal`].
pub fn reveal(strip: Id, tab: Id) -> Task<f32> {
    iced_runtime::task::widget(FindTab {
        strip,
        tab,
        viewport: None,
        target: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::Size;

    fn viewport() -> Rectangle {
        Rectangle {
            x: 100.0,
            y: 0.0,
            width: 200.0,
            height: 24.0,
        }
    }

    fn tab(x: f32, width: f32) -> Rectangle {
        Rectangle {
            x,
            y: 0.0,
            width,
            height: 24.0,
        }
    }

    #[test]
    fn a_visible_tab_needs_no_scroll() {
        // Tab spans content offsets 50..110 while the viewport shows 0..200.
        assert_eq!(reveal_offset(viewport(), 0.0, tab(150.0, 60.0)), None);
    }

    #[test]
    fn a_tab_past_the_right_edge_scrolls_just_into_view() {
        // Content offsets 250..310, viewport shows 0..200: the right edge
        // (plus margin) must land at the viewport's right edge.
        assert_eq!(
            reveal_offset(viewport(), 0.0, tab(350.0, 60.0)),
            Some(310.0 + REVEAL_PADDING - 200.0)
        );
    }

    #[test]
    fn a_tab_before_the_left_edge_scrolls_back() {
        // Content offsets 40..100 with the strip scrolled to 120: the left
        // edge (minus margin) becomes the new offset.
        assert_eq!(
            reveal_offset(viewport(), 120.0, tab(140.0, 60.0)),
            Some(40.0 - REVEAL_PADDING)
        );
    }

    #[test]
    fn the_first_tab_never_produces_a_negative_offset() {
        assert_eq!(reveal_offset(viewport(), 30.0, tab(100.0, 60.0)), Some(0.0));
    }

    #[test]
    fn grounded_press_points_measure_the_deadband_in_window_space() {
        // A press on a strip scrolled 50px right: the press surface reports
        // content-space (120, 10); the window-space location is (70, 10).
        let press = ground_to_window(Point::new(120.0, 10.0), 50.0);
        assert_eq!(press, Point::new(70.0, 10.0));
        // A tracked window-space wiggle a few pixels away stays a click —
        // ungrounded, the same wiggle would read as 50px of travel and
        // promote instantly.
        let sample = Point::new(74.0, 12.0);
        assert!(sample.distance(press) <= crate::pane_drag::DRAG_DEADBAND);
        assert!(sample.distance(Point::new(120.0, 10.0)) > crate::pane_drag::DRAG_DEADBAND);
        // An unscrolled strip grounds to itself.
        assert_eq!(
            ground_to_window(Point::new(120.0, 10.0), 0.0),
            Point::new(120.0, 10.0)
        );
    }

    /// A minimal `operation::Scrollable` recording the offset it was
    /// scrolled to.
    #[derive(Default)]
    struct MockScrollable {
        scrolled_to: Option<operation::scrollable::AbsoluteOffset<Option<f32>>>,
    }

    impl operation::Scrollable for MockScrollable {
        fn snap_to(&mut self, _offset: operation::scrollable::RelativeOffset<Option<f32>>) {}

        fn scroll_to(&mut self, offset: operation::scrollable::AbsoluteOffset<Option<f32>>) {
            self.scrolled_to = Some(offset);
        }

        fn scroll_by(
            &mut self,
            _offset: operation::scrollable::AbsoluteOffset,
            _bounds: Rectangle,
            _content_bounds: Rectangle,
        ) {
        }
    }

    #[test]
    fn reveal_scrolls_and_emits_the_same_offset() {
        let strip = Id::new("strip");
        let mut find = FindTab {
            strip: strip.clone(),
            tab: Id::new("tab"),
            viewport: None,
            target: None,
        };
        // The tab spans content offsets 250..310 while the viewport shows
        // 0..200 — the same geometry as the right-edge reveal_offset test.
        let mut state = MockScrollable::default();
        find.scrollable(
            Some(&strip),
            viewport(),
            Rectangle::new(Point::new(100.0, 0.0), Size::new(600.0, 24.0)),
            Vector::new(0.0, 0.0),
            &mut state,
        );
        find.container(Some(&Id::new("tab")), tab(350.0, 60.0));
        let expected = reveal_offset(viewport(), 0.0, tab(350.0, 60.0)).unwrap();

        let operation::Outcome::Chain(mut apply) = Operation::<f32>::finish(&find) else {
            panic!("an off-screen tab must chain the apply phase");
        };
        apply.scrollable(
            Some(&strip),
            viewport(),
            Rectangle::new(Point::new(100.0, 0.0), Size::new(600.0, 24.0)),
            Vector::new(0.0, 0.0),
            &mut state,
        );
        // The scroll the widget receives and the offset the mirror learns
        // are the same number.
        assert_eq!(
            state.scrolled_to.map(|offset| offset.x),
            Some(Some(expected))
        );
        let operation::Outcome::Some(emitted) = apply.finish() else {
            panic!("the apply phase must emit the applied offset");
        };
        assert_eq!(emitted, expected);
    }

    #[test]
    fn reveal_of_a_visible_tab_neither_scrolls_nor_emits() {
        let strip = Id::new("strip");
        let mut find = FindTab {
            strip: strip.clone(),
            tab: Id::new("tab"),
            viewport: None,
            target: None,
        };
        let mut state = MockScrollable::default();
        find.scrollable(
            Some(&strip),
            viewport(),
            Rectangle::new(Point::new(100.0, 0.0), Size::new(600.0, 24.0)),
            Vector::new(0.0, 0.0),
            &mut state,
        );
        find.container(Some(&Id::new("tab")), tab(150.0, 60.0));
        assert!(matches!(
            Operation::<f32>::finish(&find),
            operation::Outcome::None
        ));
        assert!(state.scrolled_to.is_none());
    }
}

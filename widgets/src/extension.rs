use std::{cell::RefCell, ffi::CStr, sync::Arc};

use crate::{WidgetMessage, WidgetRoot};
use deno_core::{GarbageCollected, OpState, ascii_str, op2, v8};
use iced::alignment::{Horizontal, Vertical};
use smudgy_cloud::{Mapper, Node, StoreBindings, WidgetIsolate, WidgetsEnabled};
use std::sync::atomic::{AtomicU64, Ordering};

/// Thrown when an isolate without the `widgets` smudgy capability mounts/removes a widget
/// (see `smudgy/script/PACKAGE-ISOLATES-OP-CAPABILITIES.md`). Same `NotCapable`-style message + generic
/// class as the `smudgy_ops` gate, so author debugging is uniform across all the gated ops.
#[derive(Debug, deno_core::thiserror::Error, deno_error::JsError)]
#[class(generic)]
#[error("smudgy: this package did not request the 'widgets' capability")]
struct WidgetsNotCapable;

/// Whether this isolate may create/alter on-screen widgets — the `widgets` grant `core` places in
/// `OpState` as [`WidgetsEnabled`] (`true` for the main/trusted/granted isolate; `false`/absent for a
/// sandbox that didn't request it). Only the two ops that actually mount/unmount a widget into the
/// live root are gated: the builder ops (`build_column`/`build_text`/…) only assemble a detached
/// element tree, which has no on-screen effect until one of the gated ops attaches it — so gating the
/// mount points fully enforces the capability (mirroring the mapper's gate-the-entry-ops approach).
fn ensure_widgets(state: &OpState) -> Result<(), WidgetsNotCapable> {
    if state.try_borrow::<WidgetsEnabled>().is_some_and(|w| w.0) {
        Ok(())
    } else {
        Err(WidgetsNotCapable)
    }
}

#[derive(Clone)]
struct Element {
    view_fn: Arc<dyn Fn() -> iced::Element<'static, WidgetMessage, smudgy_theme::Theme, iced::Renderer>>,
    _drop_cleanup: Arc<ElementDropCleanup>,
}

struct ElementDropCleanup {
    cleanup: Option<Box<dyn Fn() + 'static>>,
}

impl Element {
    fn new(
        f: impl Fn() -> iced::Element<'static, WidgetMessage, smudgy_theme::Theme, iced::Renderer> + 'static,
    ) -> Self {
        Self::with_cleanup(f, None)
    }

    fn with_cleanup(
        f: impl Fn() -> iced::Element<'static, WidgetMessage, smudgy_theme::Theme, iced::Renderer> + 'static,
        cleanup: Option<Box<dyn Fn() + 'static>>,
    ) -> Self {
        Self {
            view_fn: Arc::new(f),
            _drop_cleanup: Arc::new(ElementDropCleanup::new(cleanup)),
        }
    }

    fn element(&self) -> iced::Element<'static, WidgetMessage, smudgy_theme::Theme, iced::Renderer> {
        (self.view_fn)()
    }
}

impl ElementDropCleanup {
    fn new(cleanup: Option<Box<dyn Fn() + 'static>>) -> Self {
        Self { cleanup }
    }
}

impl Drop for ElementDropCleanup {
    fn drop(&mut self) {
        if let Some(cleanup) = self.cleanup.take() {
            (cleanup)();
        }
    }
}
struct ElementList(pub RefCell<Vec<Element>>);
type SmudgyWidgetRoot = WidgetRoot<'static, smudgy_theme::Theme, iced::Renderer>;
static NEXT_MAP_WIDGET_ID: AtomicU64 = AtomicU64::new(0);
static NEXT_TEXT_EDITOR_ID: AtomicU64 = AtomicU64::new(0);

type ProgressBar = iced::widget::ProgressBar<'static, smudgy_theme::Theme>;
type Column = iced::widget::Column<'static, WidgetMessage, smudgy_theme::Theme, iced::Renderer>;
type Container = iced::widget::Container<'static, WidgetMessage, smudgy_theme::Theme, iced::Renderer>;
type Row = iced::widget::Row<'static, WidgetMessage, smudgy_theme::Theme, iced::Renderer>;
type Button = iced::widget::Button<'static, WidgetMessage, smudgy_theme::Theme, iced::Renderer>;
type Stack = iced::widget::Stack<'static, WidgetMessage, smudgy_theme::Theme, iced::Renderer>;
type Scrollable =
    iced::widget::Scrollable<'static, WidgetMessage, smudgy_theme::Theme, iced::Renderer>;

unsafe impl GarbageCollected for Element {
    fn get_name(&self) -> &'static CStr {
        c"SmudgyWidgetElement"
    }

    fn trace(&self, _visitor: &mut v8::cppgc::Visitor) {}
}

unsafe impl GarbageCollected for ElementList {
    fn get_name(&self) -> &'static CStr {
        c"SmudgyWidgetElementList"
    }

    fn trace(&self, _visitor: &mut v8::cppgc::Visitor) {}
}

deno_core::extension!(
  smudgy_widgets,
  ops = [
    op_smudgy_widget_create,
    op_smudgy_widget_remove,
    op_smudgy_widget_set_enabled,
    op_smudgy_widget_list,
    op_smudgy_widget_exists,
    op_smudgy_widget_isolate_token,
    op_smudgy_widget_build_element_list,
    op_smudgy_widget_push_element,
    op_smudgy_widget_build_column,
    op_smudgy_widget_build_container,
    op_smudgy_widget_build_row,
    op_smudgy_widget_build_stack,
    op_smudgy_widget_build_text,
    op_smudgy_widget_build_progress_bar,
    op_smudgy_widget_build_button,
    op_smudgy_widget_build_scrollable,
    op_smudgy_widget_build_markdown,
    op_smudgy_widget_build_modal,
    op_smudgy_widget_build_text_editor,
    op_smudgy_widget_build_map_view,
    op_smudgy_widget_extract_markdown_links,
  ],
  esm_entry_point = "ext:smudgy_widgets/widgets.ts",
  esm = [ dir "src/extension/ts", "widgets.ts" ],
  options = {
    widget_root: SmudgyWidgetRoot,
    mapper: Option<Mapper>
  },
  state = |state, options| {
    state.put::<SmudgyWidgetRoot>(options.widget_root);
    state.put::<Option<Mapper>>(options.mapper);
  },
);

macro_rules! get_number_prop {
    ($scope:ident, $obj:ident, $name:expr) => {{
        let prop = ascii_str!($name)
            .v8_string($scope)
            .expect("Could not allocate string")
            .into();
        let value = $obj
            .get($scope, prop)
            .and_then(|v| v.to_number($scope))
            .and_then(|v| v.number_value($scope));
        value.filter(|v| v.is_finite())
    }};
}

macro_rules! get_v8_function_prop {
    ($scope:ident, $obj:ident, $name:expr) => {{
        let prop = ascii_str!($name)
            .v8_string($scope)
            .expect("Could not allocate string")
            .into();
        $obj.get($scope, prop).and_then(|v| {
            v8::Local::<v8::Function>::try_from(v)
                .map(|v| v8::Global::new($scope, v))
                .ok()
        })
    }};
}

macro_rules! get_string_prop {
    ($scope:ident, $obj:ident, $name:expr) => {{
        let prop = ascii_str!($name)
            .v8_string($scope)
            .expect("Could not allocate string")
            .into();
        $obj.get($scope, prop)
            .map(|v| v.to_rust_string_lossy($scope))
    }};
}

// Like `get_string_prop!`, but yields `None` for a missing/`undefined`/`null` prop instead of the
// literal string "undefined". Use where absent must be distinguishable from a real string (e.g. a
// `TextEditor`'s `value`/`id`, where `value={area.data(key)}` is `undefined` for an unset key).
macro_rules! get_opt_string_prop {
    ($scope:ident, $obj:ident, $name:expr) => {{
        let prop = ascii_str!($name)
            .v8_string($scope)
            .expect("Could not allocate string")
            .into();
        $obj.get($scope, prop)
            .filter(|v| v.is_string())
            .map(|v| v.to_rust_string_lossy($scope))
    }};
}

macro_rules! get_bool_prop {
    ($scope:ident, $obj:ident, $name:expr) => {{
        let prop = ascii_str!($name)
            .v8_string($scope)
            .expect("Could not allocate string")
            .into();
        $obj.get($scope, prop).map(|v| v.boolean_value($scope))
    }};
}

macro_rules! get_length_prop {
    ($scope:ident, $obj:ident, $name:expr) => {{
        let prop = ascii_str!($name)
            .v8_string($scope)
            .expect("Could not allocate string")
            .into();
        $obj.get($scope, prop).and_then(|v| {
            if v.is_number() {
                let number = v
                    .to_number($scope)
                    .and_then(|v| v.number_value($scope))
                    .unwrap_or(0.0);
                Some(iced::Length::Fixed(number as f32))
            } else if v.is_string() {
                if v.strict_equals(
                    ascii_str!("fill")
                        .v8_string($scope)
                        .expect("Could not allocate string")
                        .into(),
                ) {
                    Some(iced::Length::Fill)
                } else if v.strict_equals(
                    ascii_str!("shrink")
                        .v8_string($scope)
                        .expect("Could not allocate string")
                        .into(),
                ) {
                    Some(iced::Length::Shrink)
                } else {
                    let number = v
                        .to_number($scope)
                        .and_then(|v| v.number_value($scope))
                        .unwrap_or(0.0);
                    Some(iced::Length::Fixed(number as f32))
                }
            } else {
                None
            }
        })
    }};
}

macro_rules! get_horizontal_prop {
    ($scope:ident, $obj:ident, $name:expr) => {{
        get_string_prop!($scope, $obj, $name).and_then(|value| match value.as_str() {
            "left" | "start" => Some(Horizontal::Left),
            "center" => Some(Horizontal::Center),
            "right" | "end" => Some(Horizontal::Right),
            _ => None,
        })
    }};
}

macro_rules! get_vertical_prop {
    ($scope:ident, $obj:ident, $name:expr) => {{
        get_string_prop!($scope, $obj, $name).and_then(|value| match value.as_str() {
            "top" | "start" => Some(Vertical::Top),
            "center" => Some(Vertical::Center),
            "bottom" | "end" => Some(Vertical::Bottom),
            _ => None,
        })
    }};
}

// Panic-safe: scripts pass arbitrary strings.
macro_rules! iced_color_from_maybe_v8_string {
    ($str:expr) => {
        $str.and_then(|b| smudgy_cloud::parse_css_color(&b))
    };
}

// ---- Store bindings (interop.md §7) ---------------------------------------------
// A script's `handle.bind(path?)` token is plain frozen data carrying a host-minted id. When a
// prop value is such a token, the build op resolves the id to its shared value cell (seeded in
// `OpState` by core, like `WidgetIsolate`) and the render closure re-reads the cell every
// frame — the session store writes the cell and wakes the UI at each flush, so bound props
// repaint without a V8 tick, latest-wins per frame.

/// A prop bound to a session-store path: the shared cell plus the token's parsed `fallback`
/// (used when the bound value is null/absent) and `format` (a display template for text
/// positions, `{}` replaced by the value). The fallback is converted to the cell's [`Node`]
/// shape once, here at token resolution, so per-frame reads compare like with like.
struct BoundProp {
    cell: Arc<smudgy_cloud::StoreBindingCell>,
    fallback: Option<Node>,
    format: Option<String>,
}

impl BoundProp {
    /// The binding rendered as bare display text: strings unquoted, numbers/bools in their
    /// JSON spelling, null/absent as `""` (after `fallback`), containers as JSON — then the
    /// `format` template applied.
    fn display_text(&self) -> String {
        let loaded = self.cell.load();
        let value: &Node = if loaded.is_null() {
            self.fallback.as_ref().unwrap_or(&Node::Null)
        } else {
            &loaded
        };
        let text = match value {
            Node::Null => String::new(),
            Node::String(s) => s.to_string(),
            // `to_json` rather than `to_string`: `Node`'s `Display` routes through `to_json`
            // anyway, so calling it directly emits the same text in one allocation instead of
            // two — this runs in the render closure every frame.
            other => other.to_json(),
        };
        match &self.format {
            Some(template) => template.replacen("{}", &text, 1),
            None => text,
        }
    }
}

/// Whether `value` has a binding token's shape (whether or not its id still resolves).
fn is_binding_token(scope: &mut v8::PinScope, value: v8::Local<v8::Value>) -> bool {
    let Ok(obj) = v8::Local::<v8::Object>::try_from(value) else {
        return false;
    };
    let key = ascii_str!("__smudgyStoreBinding")
        .v8_string(scope)
        .expect("Could not allocate string")
        .into();
    obj.get(scope, key).is_some_and(|id| id.is_number())
}

/// Resolve a prop value to its [`BoundProp`] when it is a binding token. `None` when it is
/// not a token, and also for a stale id (a token minted by a previous engine generation —
/// its widgets were cleared with the engine, so this is a warn-and-degrade path, not an
/// author-facing error).
fn bound_prop_from_v8(
    scope: &mut v8::PinScope,
    state: &OpState,
    value: v8::Local<v8::Value>,
) -> Option<BoundProp> {
    let obj = v8::Local::<v8::Object>::try_from(value).ok()?;
    let key = ascii_str!("__smudgyStoreBinding")
        .v8_string(scope)
        .expect("Could not allocate string")
        .into();
    let id = obj.get(scope, key).filter(|id| id.is_number())?;
    let id = id.uint32_value(scope)?;
    let Some(cell) = state
        .try_borrow::<StoreBindings>()
        .and_then(|bindings| bindings.cell(id))
    else {
        log::warn!("smudgy widgets: unknown store-binding token id {id}; rendering it as absent");
        return None;
    };
    let fallback = get_opt_string_prop!(scope, obj, "fallback")
        .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
        .map(Node::from);
    let format = get_opt_string_prop!(scope, obj, "format");
    Some(BoundProp {
        cell,
        fallback,
        format,
    })
}

/// A widget prop that is either a build-time constant or a live store binding resolved on
/// every render. `get` returning `None` (an unparseable bound value with no usable fallback)
/// leaves the widget's own default in effect for that frame.
enum DynProp<T> {
    Static(T),
    Bound {
        prop: BoundProp,
        parse: fn(&Node) -> Option<T>,
    },
}

impl<T: Clone> DynProp<T> {
    fn get(&self) -> Option<T> {
        match self {
            Self::Static(value) => Some(value.clone()),
            Self::Bound { prop, parse } => {
                let loaded = prop.cell.load();
                parse(&loaded).or_else(|| prop.fallback.as_ref().and_then(parse))
            }
        }
    }
}

// The `DynProp::Bound` parse fns: how a store value lands in each prop type. Truncating
// f64 → f32 is the same conversion every static prop path already applies.
#[allow(clippy::cast_possible_truncation)]
fn f32_from_value(value: &Node) -> Option<f32> {
    value.as_f64().map(|number| number as f32)
}

#[allow(clippy::cast_possible_truncation)]
fn length_from_value(value: &Node) -> Option<iced::Length> {
    match value {
        Node::Number(number) => {
            number.as_f64().map(|number| iced::Length::Fixed(number as f32))
        }
        Node::String(text) => match &**text {
            "fill" => Some(iced::Length::Fill),
            "shrink" => Some(iced::Length::Shrink),
            other => other.parse::<f32>().ok().map(iced::Length::Fixed),
        },
        _ => None,
    }
}

fn color_from_value(value: &Node) -> Option<iced::Color> {
    value.as_str().and_then(smudgy_cloud::parse_css_color)
}

// Binding-aware twins of the static prop macros: a binding token resolves to `Bound` (read
// per render), anything else takes the exact static path the old macro took. An absent prop
// is `None` either way, so the `if let Some(...)` attr-fn pattern is unchanged at call sites.
macro_rules! get_dyn_f32_prop {
    ($scope:ident, $state:ident, $obj:ident, $name:expr) => {{
        let prop = ascii_str!($name)
            .v8_string($scope)
            .expect("Could not allocate string")
            .into();
        $obj.get($scope, prop).and_then(|v| {
            if let Some(bound) = bound_prop_from_v8($scope, $state, v) {
                Some(DynProp::Bound { prop: bound, parse: f32_from_value })
            } else {
                v.to_number($scope)
                    .and_then(|v| v.number_value($scope))
                    .filter(|v| v.is_finite())
                    .map(|v| {
                        #[allow(clippy::cast_possible_truncation)]
                        let value = v as f32;
                        DynProp::Static(value)
                    })
            }
        })
    }};
}

macro_rules! get_dyn_length_prop {
    ($scope:ident, $state:ident, $obj:ident, $name:expr) => {{
        let prop = ascii_str!($name)
            .v8_string($scope)
            .expect("Could not allocate string")
            .into();
        if let Some(v) = $obj.get($scope, prop) {
            if let Some(bound) = bound_prop_from_v8($scope, $state, v) {
                Some(DynProp::Bound { prop: bound, parse: length_from_value })
            } else {
                get_length_prop!($scope, $obj, $name).map(DynProp::Static)
            }
        } else {
            None
        }
    }};
}

macro_rules! get_dyn_color_prop {
    ($scope:ident, $state:ident, $obj:ident, $name:expr) => {{
        let prop = ascii_str!($name)
            .v8_string($scope)
            .expect("Could not allocate string")
            .into();
        if let Some(v) = $obj.get($scope, prop) {
            if let Some(bound) = bound_prop_from_v8($scope, $state, v) {
                Some(DynProp::Bound { prop: bound, parse: color_from_value })
            } else {
                iced_color_from_maybe_v8_string!(get_string_prop!($scope, $obj, $name))
                    .map(DynProp::Static)
            }
        } else {
            None
        }
    }};
}

/// Mount (or replace) a named widget. `target_name_id` is the hosting pane's interned name id
/// (see `smudgy_core`'s pane registry); a negative value mounts into the untargeted overlay over
/// the session's main pane. The id arrives pre-validated — `widgets.ts` resolves the
/// `createWidget` `pane` option through `op_smudgy_pane_resolve` first — and is matched against
/// live panes at render time, so a stale id renders nothing rather than erroring.
#[op2(fast)]
fn op_smudgy_widget_create(
    state: &mut OpState,
    #[string] creator: &str,
    #[string] name: &str,
    #[cppgc] widget: &Element,
    target_name_id: i32,
) -> Result<(), WidgetsNotCapable> {
    ensure_widgets(state)?;
    let target = u32::try_from(target_name_id).ok();
    let widget_root = state.borrow::<SmudgyWidgetRoot>();
    WidgetRoot::insert(widget_root, creator, name, widget.view_fn.clone(), target);
    Ok(())
}

#[op2(fast)]
fn op_smudgy_widget_remove(
    state: &mut OpState,
    #[string] creator: &str,
    #[string] name: &str,
) -> Result<(), WidgetsNotCapable> {
    ensure_widgets(state)?;
    let widget_root = state.borrow::<SmudgyWidgetRoot>();
    widget_root.remove(creator, name);
    Ok(())
}

#[op2(fast)]
fn op_smudgy_widget_set_enabled(
    state: &mut OpState,
    #[string] creator: &str,
    #[string] name: &str,
    enabled: bool,
) -> Result<(), WidgetsNotCapable> {
    ensure_widgets(state)?;
    let widget_root = state.borrow::<SmudgyWidgetRoot>();
    widget_root.set_enabled(creator, name, enabled);
    Ok(())
}

// Registry reads (`session.widgets`-style): origin-scoped by `creator`, so a package only ever
// sees its own widgets. Ungated — listing your own widgets is not a capability concern.
#[op2]
#[serde]
fn op_smudgy_widget_list(state: &mut OpState, #[string] creator: &str) -> Vec<String> {
    state.borrow::<SmudgyWidgetRoot>().list(creator)
}

#[op2(fast)]
fn op_smudgy_widget_exists(
    state: &mut OpState,
    #[string] creator: &str,
    #[string] name: &str,
) -> bool {
    state.borrow::<SmudgyWidgetRoot>().exists(creator, name)
}

/// This isolate's routing token (see [`WidgetIsolate`]). `widgets.ts` reads it once and tags
/// button callbacks with it so `core` dispatches them back into the creating isolate.
#[op2]
#[string]
fn op_smudgy_widget_isolate_token(state: &mut OpState) -> String {
    state
        .try_borrow::<WidgetIsolate>()
        .map_or_else(|| "main".to_string(), |w| w.0.clone())
}

#[op2]
#[cppgc]
fn op_smudgy_widget_build_element_list() -> ElementList {
    ElementList(RefCell::new(Vec::new()))
}

#[op2(fast)]
fn op_smudgy_widget_push_element(#[cppgc] vec: &ElementList, #[cppgc] child: &Element) {
    vec.0.borrow_mut().push(child.clone());
}

#[op2]
#[cppgc]
fn op_smudgy_widget_build_column(
    scope: &mut v8::PinScope,
    state: &mut OpState,
    #[cppgc] children: &ElementList,
    props: v8::Local<v8::Object>,
) -> Element {
    let children = children.0.take();

    let width = get_dyn_length_prop!(scope, state, props, "width");
    let height = get_dyn_length_prop!(scope, state, props, "height");
    let spacing = get_dyn_f32_prop!(scope, state, props, "spacing");
    let padding = get_dyn_f32_prop!(scope, state, props, "padding");

    let mut attr_fns: Vec<Box<dyn Fn(Column) -> Column>> = Vec::new();

    if let Some(width) = width {
        attr_fns.push(Box::new(move |column: Column| match width.get() {
            Some(width) => column.width(width),
            None => column,
        }));
    }
    if let Some(height) = height {
        attr_fns.push(Box::new(move |column: Column| match height.get() {
            Some(height) => column.height(height),
            None => column,
        }));
    }

    if let Some(spacing) = spacing {
        attr_fns.push(Box::new(move |column: Column| match spacing.get() {
            Some(spacing) => column.spacing(spacing),
            None => column,
        }));
    }
    if let Some(padding) = padding {
        attr_fns.push(Box::new(move |column: Column| match padding.get() {
            Some(padding) => column.padding(padding),
            None => column,
        }));
    }

    Element::new(move || {
        let column = iced::widget::column(children.iter().map(Element::element));
        let column = attr_fns
            .iter()
            .fold(column, |column, attr_fn| attr_fn(column));
        column.into()
    })
}

#[op2]
#[cppgc]
fn op_smudgy_widget_build_container(
    scope: &mut v8::PinScope,
    state: &mut OpState,
    props: v8::Local<v8::Object>,
    #[cppgc] child: &Element,
) -> Element {
    let child = child.clone();
    let width = get_dyn_length_prop!(scope, state, props, "width");
    let height = get_dyn_length_prop!(scope, state, props, "height");
    let align_x = get_horizontal_prop!(scope, props, "align_x");
    let align_y = get_vertical_prop!(scope, props, "align_y");
    let background = get_dyn_color_prop!(scope, state, props, "background");

    let mut attr_fns: Vec<Box<dyn Fn(Container) -> Container>> = Vec::new();

    if let Some(width) = width {
        attr_fns.push(Box::new(move |container: Container| match width.get() {
            Some(width) => container.width(width),
            None => container,
        }));
    }
    if let Some(height) = height {
        attr_fns.push(Box::new(move |container: Container| match height.get() {
            Some(height) => container.height(height),
            None => container,
        }));
    }
    if let Some(align_x) = align_x {
        attr_fns.push(Box::new(move |container: Container| {
            container.align_x(align_x)
        }));
    }
    if let Some(align_y) = align_y {
        attr_fns.push(Box::new(move |container: Container| {
            container.align_y(align_y)
        }));
    }

    if let Some(background) = background {
        attr_fns.push(Box::new(move |container: Container| {
            match background.get() {
                Some(background) => container.style(move |_theme: &smudgy_theme::Theme| {
                    iced::widget::container::Style {
                        background: Some(iced::Background::Color(background)),
                        ..Default::default()
                    }
                }),
                None => container,
            }
        }));
    }

    Element::new(move || {
        let container = iced::widget::container(child.element());
        let container = attr_fns
            .iter()
            .fold(container, |container, attr_fn| attr_fn(container));
        container.into()
    })
}

#[op2]
#[cppgc]
fn op_smudgy_widget_build_progress_bar(
    scope: &mut v8::PinScope,
    state: &mut OpState,
    props: v8::Local<v8::Object>,
) -> Element {
    let mut attr_fns: Vec<Box<dyn Fn(ProgressBar) -> ProgressBar>> = Vec::new();

    // Range and colors resolve per render: bound props (`value={vitals.bind('hp')}` is the
    // flagship binding case) re-read their cells each frame with no rebuild.
    let min = get_dyn_f32_prop!(scope, state, props, "min");
    let max = get_dyn_f32_prop!(scope, state, props, "max");
    let value = get_dyn_f32_prop!(scope, state, props, "value");

    let background = get_dyn_color_prop!(scope, state, props, "background");
    let color = get_dyn_color_prop!(scope, state, props, "color");

    let mut width = get_dyn_length_prop!(scope, state, props, "width");
    let mut height = get_dyn_length_prop!(scope, state, props, "height");

    let is_vertical = get_bool_prop!(scope, props, "vertical").unwrap_or(false);

    if is_vertical {
        std::mem::swap(&mut width, &mut height);
    }

    if let Some(width) = width {
        attr_fns.push(Box::new(move |progress_bar: ProgressBar| {
            match width.get() {
                Some(width) => progress_bar.length(width),
                None => progress_bar,
            }
        }));
    }

    if let Some(height) = height {
        attr_fns.push(Box::new(move |progress_bar: ProgressBar| {
            match height.get() {
                Some(height) => progress_bar.girth(height),
                None => progress_bar,
            }
        }));
    }

    if is_vertical {
        attr_fns.push(Box::new(move |progress_bar: ProgressBar| {
            progress_bar.vertical()
        }));
    }

    Element::new(move || {
        let min = min.as_ref().and_then(DynProp::get).unwrap_or(0.0);
        let max = max.as_ref().and_then(DynProp::get).unwrap_or(100.0).max(min);
        let value = value
            .as_ref()
            .and_then(DynProp::get)
            .unwrap_or(0.0)
            .clamp(min, max);
        let background = background.as_ref().and_then(DynProp::get);
        let color = color.as_ref().and_then(DynProp::get);
        let progress_bar: ProgressBar = iced::widget::progress_bar(min..=max, value).style(
            move |theme: &smudgy_theme::Theme| iced::widget::progress_bar::Style {
                background: background.unwrap_or(theme.styles.general.background).into(),
                bar: color.unwrap_or(iced::Color::WHITE).into(),
                border: iced::Border::default(),
            },
        );
        let progress_bar = attr_fns
            .iter()
            .fold(progress_bar, |progress_bar, attr_fn| attr_fn(progress_bar));
        progress_bar.into()
    })
}

#[op2]
#[cppgc]
fn op_smudgy_widget_build_row(
    scope: &mut v8::PinScope,
    state: &mut OpState,
    #[cppgc] children: &ElementList,
    props: v8::Local<v8::Object>,
) -> Element {
    let children = children.0.take();

    let width = get_dyn_length_prop!(scope, state, props, "width");
    let height = get_dyn_length_prop!(scope, state, props, "height");
    let spacing = get_dyn_f32_prop!(scope, state, props, "spacing");
    let padding = get_dyn_f32_prop!(scope, state, props, "padding");

    let mut attr_fns: Vec<Box<dyn Fn(Row) -> Row>> = Vec::new();

    if let Some(width) = width {
        attr_fns.push(Box::new(move |row: Row| match width.get() {
            Some(width) => row.width(width),
            None => row,
        }));
    }
    if let Some(height) = height {
        attr_fns.push(Box::new(move |row: Row| match height.get() {
            Some(height) => row.height(height),
            None => row,
        }));
    }

    if let Some(spacing) = spacing {
        attr_fns.push(Box::new(move |row: Row| match spacing.get() {
            Some(spacing) => row.spacing(spacing),
            None => row,
        }));
    }
    if let Some(padding) = padding {
        attr_fns.push(Box::new(move |row: Row| match padding.get() {
            Some(padding) => row.padding(padding),
            None => row,
        }));
    }

    Element::new(move || {
        let row = iced::widget::row(children.iter().map(Element::element));
        let row = attr_fns.iter().fold(row, |row, attr_fn| attr_fn(row));
        row.into()
    })
}

#[op2]
#[cppgc]
fn op_smudgy_widget_build_stack(
    scope: &mut v8::PinScope,
    state: &mut OpState,
    #[cppgc] children: &ElementList,
    props: v8::Local<v8::Object>,
) -> Element {
    let children = children.0.take();

    let width = get_dyn_length_prop!(scope, state, props, "width");
    let height = get_dyn_length_prop!(scope, state, props, "height");

    let mut attr_fns: Vec<Box<dyn Fn(Stack) -> Stack>> = Vec::new();

    if let Some(width) = width {
        attr_fns.push(Box::new(move |stack: Stack| match width.get() {
            Some(width) => stack.width(width),
            None => stack,
        }));
    }
    if let Some(height) = height {
        attr_fns.push(Box::new(move |stack: Stack| match height.get() {
            Some(height) => stack.height(height),
            None => stack,
        }));
    }

    Element::new(move || {
        let stack = iced::widget::stack(children.iter().map(Element::element));
        let stack = attr_fns.iter().fold(stack, |stack, attr_fn| attr_fn(stack));
        stack.into()
    })
}

/// One piece of a `Text` widget's content: literal text, or a store binding rendered as its
/// display text each frame (`<Text>HP: {vitals.bind('hp')}</Text>` — mixed children, interop.md §7).
enum TextPart {
    Static(String),
    Bound(BoundProp),
}

#[op2]
#[cppgc]
fn op_smudgy_widget_build_text(
    scope: &mut v8::PinScope,
    state: &mut OpState,
    props: v8::Local<v8::Object>,
    parts: v8::Local<v8::Array>,
) -> Element {
    // Panic-safe parse; an absent/unparseable color leaves the theme default (matching `Container`).
    let color = get_dyn_color_prop!(scope, state, props, "color");
    // Text size in pixels; absent leaves the theme default.
    let size = get_dyn_f32_prop!(scope, state, props, "size");

    // `widgets.ts` sends the normalized children: strings, plus binding tokens passed through
    // verbatim. A token whose id no longer resolves (stale engine generation) renders as
    // empty text rather than its object spelling.
    let mut content: Vec<TextPart> = Vec::new();
    for index in 0..parts.length() {
        let Some(item) = parts.get_index(scope, index) else {
            continue;
        };
        if is_binding_token(scope, item) {
            match bound_prop_from_v8(scope, state, item) {
                Some(bound) => content.push(TextPart::Bound(bound)),
                None => content.push(TextPart::Static(String::new())),
            }
        } else {
            content.push(TextPart::Static(item.to_rust_string_lossy(scope)));
        }
    }
    // All-literal content assembles once at build; any bound part re-assembles per render.
    let fixed: Option<String> = if content.iter().all(|p| matches!(p, TextPart::Static(_))) {
        Some(
            content
                .iter()
                .map(|p| match p {
                    TextPart::Static(text) => text.as_str(),
                    TextPart::Bound(_) => unreachable!("all parts are static"),
                })
                .collect(),
        )
    } else {
        None
    };

    Element::new(move || {
        let assembled = match &fixed {
            Some(text) => text.clone(),
            None => content
                .iter()
                .map(|part| match part {
                    TextPart::Static(text) => text.clone(),
                    TextPart::Bound(bound) => bound.display_text(),
                })
                .collect(),
        };
        let mut text: iced::widget::Text<'static, smudgy_theme::Theme, iced::Renderer> =
            iced::widget::text(assembled);
        if let Some(color) = color.as_ref().and_then(DynProp::get) {
            text = text.color(color);
        }
        if let Some(size) = size.as_ref().and_then(DynProp::get) {
            text = text.size(size);
        }
        text.into()
    })
}

#[op2]
#[cppgc]
fn op_smudgy_widget_build_button(
    scope: &mut v8::PinScope,
    state: &mut OpState,
    props: v8::Local<v8::Object>,
    #[cppgc] child: &Element,
    #[string] isolate_token: &str,
) -> Element {
    let child = child.clone();

    let mut attr_fns: Vec<Box<dyn Fn(Button) -> Button>> = Vec::new();

    let width = get_dyn_length_prop!(scope, state, props, "width");
    if let Some(width) = width {
        attr_fns.push(Box::new(move |button: Button| match width.get() {
            Some(width) => button.width(width),
            None => button,
        }));
    }

    let height = get_dyn_length_prop!(scope, state, props, "height");
    if let Some(height) = height {
        attr_fns.push(Box::new(move |button: Button| match height.get() {
            Some(height) => button.height(height),
            None => button,
        }));
    }

    let on_press = get_v8_function_prop!(scope, props, "onPress");
    if let Some(on_press) = on_press {
        let on_press_arc = Arc::new(on_press);
        let isolate = WidgetIsolate(isolate_token.to_string());

        attr_fns.push(Box::new(move |button: Button| {
            button.on_press(WidgetMessage::InvokeCallback {
                callback: on_press_arc.clone(),
                isolate: isolate.clone(),
                args: Vec::new(),
            })
        }));
    }

    // The named emphasis variants from the theme. Script-spawned buttons overlay the terminal, so
    // an unspecified variant defaults to the low-emphasis `subtle` rather than the loud `primary`.
    let style_fn: fn(&smudgy_theme::Theme, iced::widget::button::Status) -> iced::widget::button::Style =
        match get_string_prop!(scope, props, "variant").as_deref() {
            Some("primary") => smudgy_theme::builtins::button::primary,
            Some("secondary") => smudgy_theme::builtins::button::secondary,
            Some("link") => smudgy_theme::builtins::button::link,
            _ => smudgy_theme::builtins::button::subtle,
        };

    Element::new(move || {
        let button = iced::widget::button(child.element()).style(style_fn);
        let button = attr_fns
            .iter()
            .fold(button, |button, attr_fn| attr_fn(button));
        button.into()
    })
}

#[op2]
#[cppgc]
fn op_smudgy_widget_build_scrollable(
    scope: &mut v8::PinScope,
    state: &mut OpState,
    props: v8::Local<v8::Object>,
    #[cppgc] child: &Element,
) -> Element {
    use iced::widget::scrollable::{Anchor, Direction, Scrollbar};

    let child = child.clone();

    let width = get_dyn_length_prop!(scope, state, props, "width");
    let height = get_dyn_length_prop!(scope, state, props, "height");
    let direction = get_string_prop!(scope, props, "direction");
    let anchor_end = get_string_prop!(scope, props, "anchor").is_some_and(|a| a == "end");

    let mut attr_fns: Vec<Box<dyn Fn(Scrollable) -> Scrollable>> = Vec::new();

    if let Some(width) = width {
        attr_fns.push(Box::new(move |scrollable: Scrollable| {
            match width.get() {
                Some(width) => scrollable.width(width),
                None => scrollable,
            }
        }));
    }
    if let Some(height) = height {
        attr_fns.push(Box::new(move |scrollable: Scrollable| {
            match height.get() {
                Some(height) => scrollable.height(height),
                None => scrollable,
            }
        }));
    }

    // The default direction is a vertical scrollbar, so only horizontal/both need overriding.
    let is_horizontal = direction.as_deref() == Some("horizontal");
    match direction.as_deref() {
        Some("horizontal") => attr_fns.push(Box::new(|scrollable: Scrollable| {
            scrollable.direction(Direction::Horizontal(Scrollbar::default()))
        })),
        Some("both") => attr_fns.push(Box::new(|scrollable: Scrollable| {
            scrollable.direction(Direction::Both {
                vertical: Scrollbar::default(),
                horizontal: Scrollbar::default(),
            })
        })),
        _ => {}
    }

    // `anchor: "end"` sticks the view to the bottom (or right) so growing content -- a log, a
    // streamed transcript -- keeps its newest line on screen.
    if anchor_end {
        if is_horizontal {
            attr_fns.push(Box::new(|scrollable: Scrollable| scrollable.anchor_x(Anchor::End)));
        } else {
            attr_fns.push(Box::new(|scrollable: Scrollable| scrollable.anchor_y(Anchor::End)));
        }
    }

    Element::new(move || {
        let scrollable = iced::widget::scrollable(child.element());
        let scrollable = attr_fns
            .iter()
            .fold(scrollable, |scrollable, attr_fn| attr_fn(scrollable));
        scrollable.into()
    })
}

/// Parse-once-and-intern markdown source into a `'static` item slice. `markdown::view` borrows
/// its items for the lifetime of the element it returns, but a mounted widget's render closure
/// must yield an `Element<'static>`; leaking the parsed items satisfies that. Keying the table by
/// source text de-dupes re-mounts so each distinct document is parsed and leaked at most once --
/// this is a bounded content cache (one entry per unique markdown string ever rendered), not an
/// unbounded per-frame or per-mount leak.
fn intern_markdown_items(content: &str) -> &'static [iced::widget::markdown::Item] {
    thread_local! {
        static MARKDOWN_ITEMS: RefCell<
            std::collections::HashMap<String, &'static [iced::widget::markdown::Item]>,
        > = RefCell::new(std::collections::HashMap::new());
    }

    MARKDOWN_ITEMS.with(|cache| {
        if let Some(items) = cache.borrow().get(content) {
            return *items;
        }
        // Keyed by the original source (so identical documents dedupe), but parsed after expanding
        // smudgy command autolinks so `<go north>` becomes a real link.
        let expanded = expand_command_autolinks(content);
        let items = iced::widget::markdown::Content::parse(&expanded)
            .items()
            .to_vec()
            .into_boxed_slice();
        let leaked: &'static [iced::widget::markdown::Item] = Box::leak(items);
        cache.borrow_mut().insert(content.to_string(), leaked);
        leaked
    })
}

/// Rewrites smudgy "command autolinks" -- a bare `<command>` such as `<go north>` -- into explicit
/// Markdown links (`[go north](<go north>)`) before parsing, so they render as command chips that
/// send the command. `CommonMark` has no autolink for bare or spaced text (autolinks require a URL
/// scheme and forbid spaces), so pulldown-cmark classifies `<go north>` as inline raw HTML, which
/// the widget otherwise drops silently. We run pulldown's own tokenizer (with iced's exact options,
/// so the spans match what the subsequent `Content::parse` sees) and rewrite only the inline-HTML
/// spans whose content looks like a command. That classification, by construction, leaves real
/// URL/email autolinks (separate link events), inline code, and fenced code untouched -- so prose
/// like `x < y`, `<http://x>`, and `` `<look>` `` are unaffected.
fn expand_command_autolinks(src: &str) -> std::borrow::Cow<'_, str> {
    use pulldown_cmark::{Event, Parser};

    // The common case (no angle bracket at all) skips the parse entirely.
    if !src.contains('<') {
        return std::borrow::Cow::Borrowed(src);
    }

    let options = markdown_options();

    // A command alone on its own line parses as a block (`Html`) rather than inline (`InlineHtml`),
    // so both are considered. For a block the range can include a trailing newline; trimming it (and
    // any leading indent) leaves just the `<...>` token, and rejecting any token that still contains
    // `<`, `>`, or a newline keeps multi-tag/multi-line HTML blocks out.
    let mut edits: Vec<(std::ops::Range<usize>, &str)> = Vec::new();
    for (event, range) in Parser::new_ext(src, options).into_offset_iter() {
        if !matches!(event, Event::InlineHtml(_) | Event::Html(_)) {
            continue;
        }
        let slice = &src[range.clone()];
        let token = slice.trim();
        let start = range.start + (slice.len() - slice.trim_start().len());
        let inner = token.strip_prefix('<').and_then(|s| s.strip_suffix('>'));
        if let Some(inner) = inner
            && is_command_autolink(inner)
        {
            edits.push((start..start + token.len(), inner));
        }
    }

    if edits.is_empty() {
        return std::borrow::Cow::Borrowed(src);
    }

    let mut out = String::with_capacity(src.len() + edits.len() * 8);
    let mut last = 0;
    for (range, inner) in edits {
        out.push_str(&src[last..range.start]);
        // `[inner](<inner>)`: label is the command, the angle-bracketed destination preserves spaces,
        // and the widget's default `onLink` sends it.
        out.push('[');
        out.push_str(inner);
        out.push_str("](<");
        out.push_str(inner);
        out.push_str(">)");
        last = range.end;
    }
    out.push_str(&src[last..]);
    std::borrow::Cow::Owned(out)
}

/// The exact pulldown-cmark options iced's `markdown::Content::parse` uses, so every pass over a
/// Markdown source here -- autolink expansion, link extraction -- tokenizes identically to the
/// widget's own parse.
fn markdown_options() -> pulldown_cmark::Options {
    use pulldown_cmark::Options;
    Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
        | Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS
        | Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
}

/// Whether `inner` (the text between the angle brackets of an inline-HTML span) reads as a smudgy
/// command rather than real HTML. It must be letter-led and free of the punctuation that marks an
/// HTML tag with attribute values or a closing/self-closing tag (`=`, `/`, quotes, `<`, `>`). This
/// admits word and multi-word commands (`look`, `go north`, `enter the temple`) while leaving real
/// HTML (`<a href="x">`, `</b>`, `<br/>`) and comments (`<!-- -->`) to render as before.
fn is_command_autolink(inner: &str) -> bool {
    inner.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && !inner
            .chars()
            .any(|c| matches!(c, '=' | '/' | '"' | '\'' | '<' | '>' | '\n'))
}

/// One link the Markdown widget renders: its visible text and the destination clicking it sends.
/// Serialized as `{ label, url }` -- the return shape of `extractMarkdownLinks()` in `widgets.ts`.
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
struct MarkdownLink {
    label: String,
    url: String,
}

/// Every link the Markdown widget would render for `source`, in document order -- the scripts'
/// counterpart of the widget's own pipeline. The source goes through the same
/// [`expand_command_autolinks`] pass and the same parse options as a render, and links are then
/// collected from the event stream itself -- so escapes, inline/fenced code, reference-style
/// links, and image syntax all behave exactly as they display.
///
/// The label is the link's flattened inline text (soft/hard breaks become spaces; alt text of an
/// image nested inside the label is not visible and is skipped); an empty label falls back to the
/// destination, which is what an empty link shows a click target for.
fn extract_markdown_links(source: &str) -> Vec<MarkdownLink> {
    use pulldown_cmark::{Event, Parser, Tag, TagEnd};

    let expanded = expand_command_autolinks(source);
    let mut links = Vec::new();
    // CommonMark links never nest, so one open accumulator suffices; images may nest inside a
    // link's label, so their (invisible) alt text is depth-tracked and excluded.
    let mut open: Option<MarkdownLink> = None;
    let mut image_depth: usize = 0;
    for event in Parser::new_ext(&expanded, markdown_options()) {
        match event {
            Event::Start(Tag::Link { dest_url, .. }) => {
                open = Some(MarkdownLink {
                    label: String::new(),
                    url: dest_url.into_string(),
                });
            }
            Event::End(TagEnd::Link) => {
                if let Some(mut link) = open.take() {
                    if link.label.is_empty() {
                        link.label.clone_from(&link.url);
                    }
                    links.push(link);
                }
            }
            Event::Start(Tag::Image { .. }) => image_depth += 1,
            Event::End(TagEnd::Image) => image_depth = image_depth.saturating_sub(1),
            Event::Text(text) | Event::Code(text) => {
                if image_depth == 0
                    && let Some(link) = open.as_mut()
                {
                    link.label.push_str(&text);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if image_depth == 0
                    && let Some(link) = open.as_mut()
                {
                    link.label.push(' ');
                }
            }
            _ => {}
        }
    }
    links
}

// Ungated, like the registry reads above: extracting links is pure text work with no on-screen
// effect, so it is not a `widgets` capability concern.
#[op2]
#[serde]
fn op_smudgy_widget_extract_markdown_links(#[string] source: &str) -> Vec<MarkdownLink> {
    extract_markdown_links(source)
}

/// Builds the iced `markdown::Style` from the live palette colors. The base text color is left
/// unset on `Style` (body color is applied per-span by [`SmudgyMarkdownViewer`]); only the
/// inline-code surface and the fallback link color come from here. The viewer repaints links as
/// command chips, so `link_color` is only what shows if the viewer's per-span pass is bypassed.
fn markdown_style(colors: smudgy_theme::markdown::MarkdownColors) -> iced::widget::markdown::Style {
    iced::widget::markdown::Style {
        font: iced::Font::default(),
        inline_code_highlight: iced::advanced::text::Highlight {
            background: iced::Background::Color(colors.code_background),
            border: iced::border::rounded(4),
        },
        inline_code_padding: iced::Padding {
            top: 0.0,
            right: 3.0,
            bottom: 0.0,
            left: 3.0,
        },
        inline_code_color: colors.code_foreground,
        inline_code_font: iced::Font::MONOSPACE,
        code_block_font: iced::Font::MONOSPACE,
        link_color: colors.link,
    }
}

/// A `markdown::Viewer` that post-processes iced's default styled spans to give smudgy's Markdown
/// its three departures from the stock look:
///
/// - **Links render as command chips** -- distinct color + a subtle rounded background + a
///   monospace font + an underline. iced's built-in `Style` only exposes a link *color*, so the
///   chip treatment has to be applied to the (public) `text::Span` fields here.
/// - **Body text is pinned to the terminal foreground.** Default body spans carry no color and
///   would otherwise inherit the brighter app-chrome text color; pinning them keeps Markdown prose
///   matching server text.
/// - **Code blocks are a dark-grey panel** with light-grey text, regardless of the active scheme.
///
/// Inline-code spans already carry their color/background/font from [`markdown_style`], so the
/// per-span pass leaves them untouched.
struct SmudgyMarkdownViewer {
    colors: smudgy_theme::markdown::MarkdownColors,
}

impl SmudgyMarkdownViewer {
    /// Clones the cached, style-resolved spans for a run of text and applies smudgy's overrides:
    /// links become chips, uncolored (plain/bold/italic) spans get the body color, and already
    /// colored spans (inline code) pass through.
    fn restyle(
        &self,
        text: &iced::widget::markdown::Text,
        style: &iced::widget::markdown::Style,
    ) -> Vec<iced::advanced::text::Span<'static, iced::widget::markdown::Uri>> {
        text.spans(*style)
            .iter()
            .cloned()
            .map(|mut span| {
                if span.link.is_some() {
                    span.color = Some(self.colors.link);
                    span.font = Some(iced::Font::MONOSPACE);
                    span.underline = true;
                    span.highlight = Some(iced::advanced::text::Highlight {
                        background: iced::Background::Color(self.colors.link_background),
                        border: iced::border::rounded(3),
                    });
                    span.padding = iced::Padding {
                        top: 0.0,
                        right: 2.0,
                        bottom: 0.0,
                        left: 2.0,
                    };
                } else if span.color.is_none() {
                    span.color = Some(self.colors.body);
                }
                span
            })
            .collect()
    }
}

impl<'a> iced::widget::markdown::Viewer<'a, iced::widget::markdown::Uri, smudgy_theme::Theme, iced::Renderer>
    for SmudgyMarkdownViewer
{
    fn on_link_click(url: iced::widget::markdown::Uri) -> iced::widget::markdown::Uri {
        url
    }

    fn paragraph(
        &self,
        settings: iced::widget::markdown::Settings,
        text: &iced::widget::markdown::Text,
    ) -> iced::Element<'a, iced::widget::markdown::Uri, smudgy_theme::Theme, iced::Renderer> {
        iced::widget::rich_text(self.restyle(text, &settings.style))
            .size(settings.text_size)
            .on_link_click(Self::on_link_click)
            .into()
    }

    fn heading(
        &self,
        settings: iced::widget::markdown::Settings,
        level: &'a iced::widget::markdown::HeadingLevel,
        text: &'a iced::widget::markdown::Text,
        index: usize,
    ) -> iced::Element<'a, iced::widget::markdown::Uri, smudgy_theme::Theme, iced::Renderer> {
        use iced::widget::markdown::HeadingLevel;
        let size = match level {
            HeadingLevel::H1 => settings.h1_size,
            HeadingLevel::H2 => settings.h2_size,
            HeadingLevel::H3 => settings.h3_size,
            HeadingLevel::H4 => settings.h4_size,
            HeadingLevel::H5 => settings.h5_size,
            HeadingLevel::H6 => settings.h6_size,
        };
        // Match the default viewer's top padding so headings keep their breathing room.
        let top = if index > 0 { settings.text_size.0 / 2.0 } else { 0.0 };
        iced::widget::container(
            iced::widget::rich_text(self.restyle(text, &settings.style))
                .size(size)
                .on_link_click(Self::on_link_click),
        )
        .padding(iced::Padding {
            top,
            right: 0.0,
            bottom: 0.0,
            left: 0.0,
        })
        .into()
    }

    fn code_block(
        &self,
        settings: iced::widget::markdown::Settings,
        _language: Option<&'a str>,
        _code: &'a str,
        lines: &'a [iced::widget::markdown::Text],
    ) -> iced::Element<'a, iced::widget::markdown::Uri, smudgy_theme::Theme, iced::Renderer> {
        let text_color = self.colors.code_foreground;
        let panel_background = self.colors.code_background;

        let rows = lines.iter().map(move |line| {
            // Pin only uncolored spans: syntax-highlighted spans already carry their own color.
            let spans: Vec<_> = line
                .spans(settings.style)
                .iter()
                .cloned()
                .map(|mut span| {
                    if span.color.is_none() {
                        span.color = Some(text_color);
                    }
                    span
                })
                .collect();
            iced::Element::from(
                iced::widget::rich_text(spans)
                    .on_link_click(Self::on_link_click)
                    .font(settings.style.code_block_font)
                    .size(settings.code_size),
            )
        });

        iced::widget::container(
            iced::widget::scrollable(
                iced::widget::container(iced::widget::Column::with_children(rows))
                    .padding(settings.code_size),
            )
            .direction(iced::widget::scrollable::Direction::Horizontal(
                iced::widget::scrollable::Scrollbar::default()
                    .width(settings.code_size / 2)
                    .scroller_width(settings.code_size / 2),
            )),
        )
        .width(iced::Length::Fill)
        .padding(settings.code_size / 4)
        .style(move |_theme: &smudgy_theme::Theme| iced::widget::container::Style {
            background: Some(iced::Background::Color(panel_background)),
            border: iced::border::rounded(4),
            ..iced::widget::container::Style::default()
        })
        .into()
    }

    // Lists are overridden only to color the bullet glyph / ordered-number with the pinned body
    // color; the default impls render those markers in the ambient (brighter) chrome text color,
    // which would otherwise sit next to body-pinned item text. Layout mirrors iced's defaults so
    // spacing/alignment are unchanged; item content still recurses through `self` (so links inside
    // list items get the chip treatment and nested text stays body-pinned).
    fn unordered_list(
        &self,
        settings: iced::widget::markdown::Settings,
        bullets: &'a [iced::widget::markdown::Bullet],
    ) -> iced::Element<'a, iced::widget::markdown::Uri, smudgy_theme::Theme, iced::Renderer> {
        use iced::widget::markdown::Bullet;
        let body = self.colors.body;
        let rows = bullets.iter().map(move |bullet| {
            let marker: iced::Element<'a, iced::widget::markdown::Uri, smudgy_theme::Theme, iced::Renderer> =
                match bullet {
                    Bullet::Point { .. } => iced::widget::text("\u{2022}")
                        .size(settings.text_size)
                        .color(body)
                        .into(),
                    Bullet::Task { done, .. } => iced::Element::from(
                        iced::widget::container(
                            iced::widget::checkbox(*done).size(settings.text_size),
                        )
                        .center_y(
                            iced::widget::text::LineHeight::default()
                                .to_absolute(settings.text_size),
                        ),
                    ),
                };
            let (Bullet::Point { items } | Bullet::Task { items, .. }) = bullet;
            iced::widget::Row::with_children([
                marker,
                iced::widget::markdown::view_with(
                    items,
                    iced::widget::markdown::Settings {
                        spacing: settings.spacing * 0.6,
                        ..settings
                    },
                    self,
                ),
            ])
            .spacing(settings.spacing)
            .into()
        });
        iced::widget::Column::with_children(rows)
            .spacing(settings.spacing * 0.75)
            .padding([0.0, settings.spacing.0])
            .into()
    }

    fn ordered_list(
        &self,
        settings: iced::widget::markdown::Settings,
        start: u64,
        bullets: &'a [iced::widget::markdown::Bullet],
    ) -> iced::Element<'a, iced::widget::markdown::Uri, smudgy_theme::Theme, iced::Renderer> {
        use iced::widget::markdown::Bullet;
        let body = self.colors.body;
        // Width of the number column, mirroring iced's default so multi-digit markers right-align.
        #[allow(clippy::cast_precision_loss)]
        let number_width = {
            let digits = (start + bullets.len() as u64).max(1).to_string().len();
            settings.text_size * ((digits as f32 / 2.0).ceil() + 1.0)
        };
        let rows = bullets.iter().enumerate().map(move |(i, bullet)| {
            let (Bullet::Point { items } | Bullet::Task { items, .. }) = bullet;
            iced::widget::Row::with_children([
                iced::widget::text(format!("{}.", i as u64 + start))
                    .size(settings.text_size)
                    .color(body)
                    .align_x(Horizontal::Right)
                    .width(number_width)
                    .into(),
                iced::widget::markdown::view_with(
                    items,
                    iced::widget::markdown::Settings {
                        spacing: settings.spacing * 0.6,
                        ..settings
                    },
                    self,
                ),
            ])
            .spacing(settings.spacing)
            .into()
        });
        iced::widget::Column::with_children(rows)
            .spacing(settings.spacing * 0.75)
            .into()
    }
}

#[op2]
#[cppgc]
fn op_smudgy_widget_build_markdown(
    scope: &mut v8::PinScope,
    state: &mut OpState,
    props: v8::Local<v8::Object>,
    #[string] content: &str,
    #[string] isolate_token: &str,
) -> Element {
    let items = intern_markdown_items(content);
    let size = get_dyn_f32_prop!(scope, state, props, "size");
    let on_link = get_v8_function_prop!(scope, props, "onLink").map(Arc::new);
    let isolate = WidgetIsolate(isolate_token.to_string());

    Element::new(move || {
        // Colors are read every render (not snapshotted at build), so switching the terminal scheme
        // reflows mounted Markdown without a rebuild. `current()` is a lock-free `ArcSwap` load; the
        // UI resolves these from the active terminal palette (`smudgy_theme::markdown`).
        let colors = *smudgy_theme::markdown::current();
        let settings = match size.as_ref().and_then(DynProp::get) {
            Some(size) => {
                iced::widget::markdown::Settings::with_text_size(size, markdown_style(colors))
            }
            None => iced::widget::markdown::Settings::with_style(markdown_style(colors)),
        };
        let viewer = SmudgyMarkdownViewer { colors };
        let on_link = on_link.clone();
        let isolate = isolate.clone();
        iced::widget::markdown::view_with(items.iter(), settings, &viewer).map(move |url| {
            match &on_link {
                Some(callback) => WidgetMessage::InvokeCallback {
                    callback: callback.clone(),
                    isolate: isolate.clone(),
                    args: vec![url],
                },
                None => WidgetMessage::Noop,
            }
        })
    })
}

#[op2]
#[cppgc]
fn op_smudgy_widget_build_text_editor(
    scope: &mut v8::PinScope,
    props: v8::Local<v8::Object>,
    #[string] isolate_token: &str,
) -> Element {
    // Identity key for the editor's buffer in the store. An explicit `id` (scoped to this package's
    // isolate) gives a sibling-distinct, stable key; without one, an auto key is unique per build.
    // Either way the buffer is (re)seeded to `value` on the first frame of each mount (see below),
    // so the key controls identity, not whether a stale buffer survives.
    // The token's leading field is the isolate's instance nonce (see `WidgetIsolate`), which
    // changes on every engine rebuild — key on the stable role part after it, so a reload that
    // re-mounts the same `id` reclaims its buffer instead of stranding it in the store.
    let stable_isolate = isolate_token
        .split_once('\u{1f}')
        .map_or(isolate_token, |(_, role)| role);
    let key = match get_opt_string_prop!(scope, props, "id") {
        Some(id) if !id.is_empty() => format!("{stable_isolate}\u{1f}{id}"),
        _ => format!("\u{1f}auto\u{1f}{}", NEXT_TEXT_EDITOR_ID.fetch_add(1, Ordering::Relaxed)),
    };
    let initial_text = get_opt_string_prop!(scope, props, "value").unwrap_or_default();
    let on_change = get_v8_function_prop!(scope, props, "onChange").map(Arc::new);
    let isolate = WidgetIsolate(isolate_token.to_string());

    let config = crate::text_editor::EditorConfig {
        height: get_length_prop!(scope, props, "height"),
        padding: get_number_prop!(scope, props, "padding").map(|v| v as f32),
        placeholder: get_opt_string_prop!(scope, props, "placeholder"),
        size: get_number_prop!(scope, props, "size").map(|v| v as f32),
    };

    // Best-effort cleanup, mirroring the map widget: drop the buffer when the element drops if the
    // store is reachable at that point.
    let cleanup_key = key.clone();
    let drop_cleanup = crate::text_editor::with_active_text_store(|store| {
        let store = store.clone();
        Box::new(move || store.remove_editor(&cleanup_key)) as Box<dyn Fn() + 'static>
    });

    // `value` is authoritative per mount. The build op runs on the session thread where the
    // UI-thread store isn't reachable, so we reseed on the FIRST frame of this build instead: a
    // fresh mount (e.g. a script reload that re-uses the same `id`) resets the buffer to `value`,
    // while later frames of the same mount preserve in-progress edits.
    let seeded = std::cell::Cell::new(false);

    Element::with_cleanup(
        move || {
            let key = key.clone();
            let isolate = isolate.clone();
            let on_change = on_change.clone();
            let first_frame = !seeded.replace(true);
            crate::text_editor::with_active_text_store(|store| {
                let handle = if first_frame {
                    store.seed_editor(&key, &initial_text)
                } else {
                    store.ensure_editor(&key, &initial_text)
                };
                handle
                    .element(&config)
                    .map(move |action| WidgetMessage::TextEditorAction {
                        key: key.clone(),
                        action,
                        on_change: on_change.clone(),
                        isolate: isolate.clone(),
                    })
            })
            .unwrap_or_else(|| iced::widget::text("text editor unavailable").into())
        },
        drop_cleanup,
    )
}

#[op2]
#[cppgc]
fn op_smudgy_widget_build_modal(
    scope: &mut v8::PinScope,
    state: &mut OpState,
    props: v8::Local<v8::Object>,
    #[cppgc] child: &Element,
    #[string] isolate_token: &str,
) -> Element {
    let child = child.clone();

    // A dimmed full-screen backdrop (translucent black unless overridden) under a centered content
    // box. The backdrop is `opaque` so it captures clicks -- the map/terminal beneath stay inert
    // while the modal is up -- and a `mouse_area` turns a backdrop click into the optional
    // `onDismiss`. With no `onDismiss` the backdrop still blocks input but never dismisses, so an
    // in-progress edit can't be lost to a stray click.
    let background = get_dyn_color_prop!(scope, state, props, "background");
    let on_dismiss = get_v8_function_prop!(scope, props, "onDismiss").map(Arc::new);
    let isolate = WidgetIsolate(isolate_token.to_string());

    Element::new(move || {
        let background = background
            .as_ref()
            .and_then(DynProp::get)
            .unwrap_or(iced::Color {
                a: 0.6,
                ..iced::Color::BLACK
            });
        let backdrop = iced::widget::container(iced::widget::space::horizontal())
            .width(iced::Length::Fill)
            .height(iced::Length::Fill)
            .style(move |_theme: &smudgy_theme::Theme| iced::widget::container::Style {
                background: Some(iced::Background::Color(background)),
                ..Default::default()
            });
        let mut backdrop = iced::widget::mouse_area(backdrop);
        if let Some(on_dismiss) = &on_dismiss {
            backdrop = backdrop.on_press(WidgetMessage::InvokeCallback {
                callback: on_dismiss.clone(),
                isolate: isolate.clone(),
                args: Vec::new(),
            });
        }

        let layers: Vec<iced::Element<'static, WidgetMessage, smudgy_theme::Theme, iced::Renderer>> = vec![
            iced::widget::opaque(backdrop),
            iced::widget::center(child.element()).into(),
        ];
        iced::widget::stack(layers).into()
    })
}

#[op2]
#[cppgc]
fn op_smudgy_widget_build_map_view(state: &mut OpState) -> Element {
    let mapper = state.borrow::<Option<Mapper>>().clone();
    let widget_id = NEXT_MAP_WIDGET_ID.fetch_add(1, Ordering::Relaxed);

    let drop_cleanup = crate::map::with_active_store(|store| {
        let store = store.clone();
        Box::new(move || store.remove_map(widget_id)) as Box<dyn Fn() + 'static>
    });

    Element::with_cleanup(
        move || {
            if let Some(mapper) = mapper.clone() {
                crate::map::with_active_store(|store| {
                    let handle = store.ensure_map(mapper.clone(), widget_id);
                    Some(handle.element().map(move |message| {
                        crate::WidgetMessage::MapMessage {
                            id: widget_id,
                            message,
                        }
                    }))
                })
                .flatten()
                .unwrap_or_else(|| iced::widget::text("map unavailable").into())
            } else {
                iced::widget::text("map unavailable (no mapper)").into()
            }
        },
        drop_cleanup,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};

    #[test]
    fn element_cleanup_runs_on_last_drop() {
        let cleaned = Rc::new(Cell::new(false));
        {
            let flag = Rc::clone(&cleaned);
            let element = Element::with_cleanup(
                || iced::widget::text("test").into(),
                Some(Box::new(move || flag.set(true))),
            );
            let another = element.clone();
            drop(another);
        }
        assert!(cleaned.get());
    }

    #[test]
    fn command_autolinks_become_links() {
        assert_eq!(
            expand_command_autolinks("Type <look> or <go north> to move."),
            "Type [look](<look>) or [go north](<go north>) to move."
        );
        assert_eq!(
            expand_command_autolinks("<enter the temple>"),
            "[enter the temple](<enter the temple>)"
        );
        // Hyphens are fine; a command on its own line still parses as inline HTML.
        assert_eq!(expand_command_autolinks("<go-north>"), "[go-north](<go-north>)");
    }

    #[test]
    fn non_commands_are_left_untouched() {
        // Prose comparisons, real URL/email autolinks, inline code, and fenced code are not
        // rewritten -- and a borrowed Cow proves no allocation happened.
        for src in [
            "Compare x < y and a > b here.",
            "Visit <http://example.com> now.",
            "Email <foo@bar.com> me.",
            "Inline `<look>` stays literal.",
            "```\n<look>\n```",
            "<say hi!>",  // `!` -> not tokenized as inline HTML
            "no angle brackets at all",
        ] {
            assert!(
                matches!(expand_command_autolinks(src), std::borrow::Cow::Borrowed(_)),
                "expected `{src}` to be left unchanged"
            );
        }
    }

    // ---- store-binding value coercion (interop.md §7) -- the per-render parse fns and the text
    // rendering, testable without a V8 runtime (the v8 token-extraction glue is exercised by
    // the app; there is still no headless widgets runtime test).

    fn bound(value: serde_json::Value) -> BoundProp {
        BoundProp {
            cell: Arc::new(smudgy_cloud::StoreBindingCell::new(value)),
            fallback: None,
            format: None,
        }
    }

    #[test]
    fn bound_display_text_renders_bare_values_fallback_and_format() {
        use serde_json::json;
        assert_eq!(bound(json!("hi")).display_text(), "hi", "strings render unquoted");
        assert_eq!(bound(json!(42.5)).display_text(), "42.5");
        assert_eq!(bound(json!(true)).display_text(), "true");
        assert_eq!(bound(json!(null)).display_text(), "", "null/absent renders empty");
        assert_eq!(bound(json!({ "a": 1 })).display_text(), r#"{"a":1}"#);

        let with_fallback = BoundProp {
            fallback: Some(Node::from(json!(0))),
            ..bound(json!(null))
        };
        assert_eq!(with_fallback.display_text(), "0", "fallback covers null/absent");

        let formatted = BoundProp {
            format: Some("{} hp".to_string()),
            ..bound(json!(7))
        };
        assert_eq!(formatted.display_text(), "7 hp");

        let live = bound(json!(1));
        live.cell.set(serde_json::json!(2));
        assert_eq!(live.display_text(), "2", "re-renders read the live cell");
    }

    #[test]
    fn binding_parse_fns_coerce_store_values() {
        use serde_json::json;
        let node = |value: serde_json::Value| Node::from(value);
        assert_eq!(f32_from_value(&node(json!(12.5))), Some(12.5));
        assert_eq!(f32_from_value(&node(json!("12"))), None, "no string-to-number coercion");

        assert_eq!(length_from_value(&node(json!(120))), Some(iced::Length::Fixed(120.0)));
        assert_eq!(length_from_value(&node(json!("fill"))), Some(iced::Length::Fill));
        assert_eq!(length_from_value(&node(json!("shrink"))), Some(iced::Length::Shrink));
        assert_eq!(length_from_value(&node(json!("64"))), Some(iced::Length::Fixed(64.0)));
        assert_eq!(length_from_value(&node(json!(null))), None);

        assert_eq!(
            color_from_value(&node(json!("#ff0000"))),
            Some(iced::Color::from_rgb(1.0, 0.0, 0.0))
        );
        assert_eq!(color_from_value(&node(json!(3))), None);
        assert_eq!(color_from_value(&node(json!("not-a-color"))), None);
    }

    #[test]
    fn dyn_prop_bound_falls_back_only_when_unparseable() {
        use serde_json::json;
        let prop = DynProp::Bound {
            prop: BoundProp {
                fallback: Some(Node::from(json!(50))),
                ..bound(json!(null))
            },
            parse: f32_from_value,
        };
        assert_eq!(prop.get(), Some(50.0), "null value -> fallback");
        if let DynProp::Bound { prop: inner, .. } = &prop {
            inner.cell.set(json!(75));
        }
        assert_eq!(prop.get(), Some(75.0), "a live value wins over the fallback");
        assert_eq!(DynProp::Static(1.0f32).get(), Some(1.0));
    }

    fn link(label: &str, url: &str) -> MarkdownLink {
        MarkdownLink {
            label: label.to_string(),
            url: url.to_string(),
        }
    }

    #[test]
    fn markdown_links_walk_every_form_in_order() {
        assert_eq!(
            extract_markdown_links(
                "Type <look>, then [the temple](<enter temple>) or [north](go-north)."
            ),
            vec![
                link("look", "look"),
                link("the temple", "enter temple"),
                link("north", "go-north"),
            ]
        );
        // Labels flatten nested inline markup; an empty label falls back to the destination.
        assert_eq!(
            extract_markdown_links("[**go** north](<go north>) [](<look>)"),
            vec![link("go north", "go north"), link("look", "look")]
        );
        // Reference-style links resolve like any render.
        assert_eq!(
            extract_markdown_links("See [the gate][g].\n\n[g]: gate-room"),
            vec![link("the gate", "gate-room")]
        );
        // Real URL autolinks are links the widget renders too.
        assert_eq!(
            extract_markdown_links("Visit <http://example.com> now."),
            vec![link("http://example.com", "http://example.com")]
        );
    }

    #[test]
    fn markdown_links_follow_the_renderer_not_a_regex() {
        // Everything a naive pattern match gets wrong: escapes, code spans, fenced code, and
        // image syntax yield no links, because the widget renders none.
        for src in [
            "\\[not a link](x)",
            "Inline `<look>` and `[a](b)` stay literal.",
            "```\n<look>\n[a](b)\n```",
            "![alt](image.png)",
            "Compare x < y and a > b here.",
        ] {
            assert_eq!(extract_markdown_links(src), vec![], "expected no links in `{src}`");
        }
        // An image nested in a link's label contributes no (invisible) alt text to the label.
        assert_eq!(
            extract_markdown_links("[![alt](i.png) enter](<enter temple>)"),
            vec![link(" enter", "enter temple")]
        );
    }

    #[test]
    fn is_command_autolink_classifies() {
        assert!(is_command_autolink("look"));
        assert!(is_command_autolink("go north"));
        assert!(is_command_autolink("enter the temple"));
        assert!(!is_command_autolink("")); // empty
        assert!(!is_command_autolink("/b")); // closing tag
        assert!(!is_command_autolink("br/")); // self-closing
        assert!(!is_command_autolink("a href=\"x\"")); // real HTML attributes
        assert!(!is_command_autolink("3 blind mice")); // not letter-led
    }
}

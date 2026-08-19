use std::{cell::RefCell, ffi::CStr, sync::Arc};

use crate::image_store::{EntryState, ImageEntryCell, ImageStore};
use crate::{WidgetMessage, WidgetRoot};
use deno_core::{GarbageCollected, OpState, ascii_str, op2, v8};
use iced::alignment::{Horizontal, Vertical};
use serde::{Deserialize, de::DeserializeOwned};
use smudgy_cloud::image_source::{
    ImageSourcePolicy, RegisteredImageCreator, ResolvedImageSource, SrcMemoKey, memo_key,
    register_creator, resolve_src,
};
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
    view_fn:
        Arc<dyn Fn() -> iced::Element<'static, WidgetMessage, smudgy_theme::Theme, iced::Renderer>>,
}

impl Element {
    fn new(
        f: impl Fn() -> iced::Element<'static, WidgetMessage, smudgy_theme::Theme, iced::Renderer>
        + 'static,
    ) -> Self {
        Self {
            view_fn: Arc::new(f),
        }
    }

    fn element(
        &self,
    ) -> iced::Element<'static, WidgetMessage, smudgy_theme::Theme, iced::Renderer> {
        (self.view_fn)()
    }
}
struct ElementList(pub RefCell<Vec<Element>>);
type SmudgyWidgetRoot = WidgetRoot<'static, smudgy_theme::Theme, iced::Renderer>;
static NEXT_MAP_WIDGET_ID: AtomicU64 = AtomicU64::new(0);
static NEXT_TEXT_EDITOR_ID: AtomicU64 = AtomicU64::new(0);

type ProgressBar = iced::widget::ProgressBar<'static, smudgy_theme::Theme>;
type Column = iced::widget::Column<'static, WidgetMessage, smudgy_theme::Theme, iced::Renderer>;
type Container =
    iced::widget::Container<'static, WidgetMessage, smudgy_theme::Theme, iced::Renderer>;
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
    op_smudgy_widget_build_canvas,
    op_smudgy_widget_build_space,
    op_smudgy_widget_build_checkbox,
    op_smudgy_widget_build_radio,
    op_smudgy_widget_build_tooltip,
    op_smudgy_widget_build_table,
    op_smudgy_widget_register_image_creator,
    op_smudgy_widget_build_image,
    op_smudgy_widget_extract_markdown_links,
  ],
  esm_entry_point = "ext:smudgy_widgets/widgets.ts",
  esm = [ dir "src/extension/ts", "widgets.ts" ],
  options = {
    widget_root: SmudgyWidgetRoot,
    mapper: Option<Mapper>,
    // Process-global; the same handle is passed to every isolate's extension init. `None`
    // in headless/test runtimes with no UI image loader — the build op then degrades every
    // `<Image>` to its placeholder.
    image_store: Option<ImageStore>
  },
  state = |state, options| {
    state.put::<SmudgyWidgetRoot>(options.widget_root);
    state.put::<Option<Mapper>>(options.mapper);
    state.put::<Option<ImageStore>>(options.image_store);
    // Per-isolate: creator tokens + the resolve memo (see `ImageRegistry`). Fresh per
    // isolate build, so tokens never collide across isolates.
    state.put::<RefCell<ImageRegistry>>(RefCell::new(ImageRegistry::default()));
  },
  customizer = |ext: &mut deno_core::Extension| {
    // deno_core 0.410 records `esm = [...]` sources as absolute build-machine
    // paths (its snapshot-first design). This extension initializes per isolate
    // OUTSIDE the startup snapshot (smudgy_script's build.rs bakes only the
    // deno_runtime base set), so the source must ship in the binary: swap the
    // path-based entry for the embedded bytes. The specifier must match
    // `esm_entry_point`.
    ext.esm_files = std::borrow::Cow::Borrowed(SMUDGY_WIDGETS_EMBEDDED_ESM);
  },
);

/// `widgets.ts` embedded for runtime (non-snapshot) extension init — see the
/// `customizer` on [`smudgy_widgets`]. 7-bit ASCII by contract
/// (`ascii_str_include!` requires it).
static SMUDGY_WIDGETS_EMBEDDED_ESM: &[deno_core::ExtensionFileSource] =
    &[deno_core::ExtensionFileSource::new(
        "ext:smudgy_widgets/widgets.ts",
        deno_core::ascii_str_include!("extension/ts/widgets.ts"),
    )];

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
        // One spelling source for value→text: `string_from_value` (strings unquoted,
        // everything else in its JSON spelling), with null/absent rendering empty — the
        // same spelling `Radio.selected` compares against.
        let text = string_from_value(value).unwrap_or_default();
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

fn serde_from_node<T: DeserializeOwned>(node: &Node) -> Result<T, serde_json::Error> {
    serde_json::to_value(node).and_then(serde_json::from_value)
}

/// Log a widget prop diagnostic once per distinct message. Bound props re-read
/// per frame and static props re-build per mount, so an unconditional warn on a
/// malformed value would repeat for as long as the value stays bad.
fn warn_once(message: String) {
    use std::sync::{LazyLock, Mutex};
    static WARNED: LazyLock<Mutex<std::collections::HashSet<String>>> =
        LazyLock::new(|| Mutex::new(std::collections::HashSet::new()));
    let mut warned = match WARNED.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if warned.insert(message.clone()) {
        log::warn!("{message}");
    }
}

/// A structured prop resolved through serde: a build-time constant, or a store
/// binding whose parse result is memoized on the snapshot's `Arc` identity —
/// an unchanged store node is never re-parsed on later renders
/// (`serde_json::to_value` deep-copies the tree, so the memo is what keeps
/// bound structured props off the per-frame hot path). Used by MapView's
/// defaultStyle/apply/doors objects.
enum SerdeProp<T> {
    Static(T),
    Bound {
        prop: BoundProp,
        name: &'static str,
        parse: fn(&Node) -> Result<T, serde_json::Error>,
        cache: RefCell<Option<(Arc<Node>, Option<T>)>>,
    },
}

impl<T: Clone> SerdeProp<T> {
    fn get(&self) -> Option<T> {
        match self {
            Self::Static(value) => Some(value.clone()),
            Self::Bound {
                prop,
                name,
                parse,
                cache,
            } => {
                let loaded = prop.cell.load();
                if let Some((snapshot, parsed)) = cache.borrow().as_ref()
                    && Arc::ptr_eq(snapshot, &loaded)
                {
                    return parsed.clone();
                }
                let parsed = Self::parse_snapshot(&loaded, prop, name, *parse);
                *cache.borrow_mut() = Some((loaded, parsed.clone()));
                parsed
            }
        }
    }

    /// Parse a fresh snapshot. A null snapshot (unset store path) silently
    /// takes the token's `fallback`; a malformed one is reported once and
    /// then also falls back.
    fn parse_snapshot(
        loaded: &Node,
        prop: &BoundProp,
        name: &str,
        parse: fn(&Node) -> Result<T, serde_json::Error>,
    ) -> Option<T> {
        if !loaded.is_null() {
            match parse(loaded) {
                Ok(parsed) => return Some(parsed),
                Err(err) => warn_once(format!(
                    "smudgy widgets: bound `{name}` value failed to parse: {err}"
                )),
            }
        }
        let fallback = prop.fallback.as_ref()?;
        match parse(fallback) {
            Ok(parsed) => Some(parsed),
            Err(err) => {
                warn_once(format!(
                    "smudgy widgets: `{name}` binding fallback failed to parse: {err}"
                ));
                None
            }
        }
    }
}

/// Resolve a structured prop from either a static JS value or a live store
/// binding. `P` is the wire shape (parsed directly by serde_v8, which keeps
/// BigInt-carried u64 halves intact on the static path), `T` the widget-side
/// value cached per snapshot; `parse` is `T`'s from-store-node reading. A
/// malformed static value is reported once and dropped rather than silently
/// nulled.
fn get_serde_prop<P, T>(
    scope: &mut v8::PinScope,
    state: &OpState,
    props: v8::Local<v8::Object>,
    name: &'static str,
    parse: fn(&Node) -> Result<T, serde_json::Error>,
    convert: fn(P) -> T,
) -> Option<SerdeProp<T>>
where
    P: DeserializeOwned,
    T: Clone,
{
    let key = v8::String::new(scope, name)?.into();
    let value = props.get(scope, key)?;
    if value.is_null_or_undefined() {
        return None;
    }
    if is_binding_token(scope, value) {
        return bound_prop_from_v8(scope, state, value).map(|prop| SerdeProp::Bound {
            prop,
            name,
            parse,
            cache: RefCell::new(None),
        });
    }
    match deno_core::serde_v8::from_v8::<P>(scope, value) {
        Ok(parsed) => Some(SerdeProp::Static(convert(parsed))),
        Err(err) => {
            warn_once(format!(
                "smudgy widgets: `{name}` prop failed to parse: {err}"
            ));
            None
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
        Node::Number(number) => number
            .as_f64()
            .map(|number| iced::Length::Fixed(number as f32)),
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

/// Strict boolean: only a JSON `true`/`false` drives a bound bool prop — no truthiness
/// coercion, so a stray string/number reads as absent (widget default) rather than "on".
fn bool_from_value(value: &Node) -> Option<bool> {
    match value {
        Node::Bool(flag) => Some(*flag),
        _ => None,
    }
}

/// A value in its JSON string form (strings unquoted); `None` for null/absent. The one
/// spelling shared by bound-text display ([`BoundProp::display_text`]) and string-compared
/// props like `Radio.selected` — so `selected={h.bind('mode')}` matches `value="fast"` for
/// a stored `"fast"` and `value="5"` for a stored `5`, exactly as a `Text` binding would
/// show them.
fn string_from_value(value: &Node) -> Option<String> {
    match value {
        Node::Null => None,
        Node::String(text) => Some(text.to_string()),
        other => Some(other.to_json()),
    }
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
                Some(DynProp::Bound {
                    prop: bound,
                    parse: f32_from_value,
                })
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
                Some(DynProp::Bound {
                    prop: bound,
                    parse: length_from_value,
                })
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
                Some(DynProp::Bound {
                    prop: bound,
                    parse: color_from_value,
                })
            } else {
                iced_color_from_maybe_v8_string!(get_string_prop!($scope, $obj, $name))
                    .map(DynProp::Static)
            }
        } else {
            None
        }
    }};
}

macro_rules! get_dyn_bool_prop {
    ($scope:ident, $state:ident, $obj:ident, $name:expr) => {{
        let prop = ascii_str!($name)
            .v8_string($scope)
            .expect("Could not allocate string")
            .into();
        $obj.get($scope, prop).and_then(|v| {
            if let Some(bound) = bound_prop_from_v8($scope, $state, v) {
                Some(DynProp::Bound {
                    prop: bound,
                    parse: bool_from_value,
                })
            } else if v.is_boolean() {
                Some(DynProp::Static(v.boolean_value($scope)))
            } else {
                None
            }
        })
    }};
}

macro_rules! get_dyn_string_prop {
    ($scope:ident, $state:ident, $obj:ident, $name:expr) => {{
        let prop = ascii_str!($name)
            .v8_string($scope)
            .expect("Could not allocate string")
            .into();
        $obj.get($scope, prop).and_then(|v| {
            if let Some(bound) = bound_prop_from_v8($scope, $state, v) {
                Some(DynProp::Bound {
                    prop: bound,
                    parse: string_from_value,
                })
            } else {
                get_opt_string_prop!($scope, $obj, $name).map(DynProp::Static)
            }
        })
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
        let max = max
            .as_ref()
            .and_then(DynProp::get)
            .unwrap_or(100.0)
            .max(min);
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

/// One piece of a text-bearing prop's content: literal text, or a store binding rendered as
/// its display text each frame (`<Text>HP: {vitals.bind('hp')}</Text>` — mixed children,
/// interop.md §7). Shared by `Text` and the labeled form widgets (`Checkbox`/`Radio`).
enum TextPart {
    Static(String),
    Bound(BoundProp),
}

/// Read a `widgets.ts`-normalized parts array (strings + binding tokens verbatim). A token
/// whose id no longer resolves (stale engine generation) renders as empty text rather than
/// its object spelling.
fn collect_text_parts(
    scope: &mut v8::PinScope,
    state: &OpState,
    parts: v8::Local<v8::Array>,
) -> Vec<TextPart> {
    let mut content = Vec::with_capacity(parts.length() as usize);
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
    content
}

/// Assemble parts into the current display string (bound parts re-read their cells).
fn assemble_text_parts(parts: &[TextPart]) -> String {
    parts
        .iter()
        .map(|part| match part {
            TextPart::Static(text) => text.clone(),
            TextPart::Bound(bound) => bound.display_text(),
        })
        .collect()
}

/// A text-bearing prop's content with the all-static assembly folded once at build:
/// entirely-literal content pays its string build a single time (the `Text` fast path,
/// shared by every labeled widget); any bound part re-assembles per render.
struct TextContent {
    parts: Vec<TextPart>,
    fixed: Option<String>,
}

impl TextContent {
    fn collect(scope: &mut v8::PinScope, state: &OpState, parts: v8::Local<v8::Array>) -> Self {
        let parts = collect_text_parts(scope, state, parts);
        let fixed = parts
            .iter()
            .all(|part| matches!(part, TextPart::Static(_)))
            .then(|| assemble_text_parts(&parts));
        Self { parts, fixed }
    }

    /// Whether any content was authored at all. A build-time fact — empty-label decisions
    /// hang off this rather than the per-frame string, so a bound part that transiently
    /// renders empty cannot flicker the label in and out of the layout.
    fn has_parts(&self) -> bool {
        !self.parts.is_empty()
    }

    fn current(&self) -> String {
        match &self.fixed {
            Some(text) => text.clone(),
            None => assemble_text_parts(&self.parts),
        }
    }
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

    let content = TextContent::collect(scope, state, parts);

    Element::new(move || {
        let assembled = content.current();
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
    let style_fn: fn(
        &smudgy_theme::Theme,
        iced::widget::button::Status,
    ) -> iced::widget::button::Style = match get_string_prop!(scope, props, "variant").as_deref() {
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
        attr_fns.push(Box::new(move |scrollable: Scrollable| match width.get() {
            Some(width) => scrollable.width(width),
            None => scrollable,
        }));
    }
    if let Some(height) = height {
        attr_fns.push(Box::new(move |scrollable: Scrollable| match height.get() {
            Some(height) => scrollable.height(height),
            None => scrollable,
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
            attr_fns.push(Box::new(|scrollable: Scrollable| {
                scrollable.anchor_x(Anchor::End)
            }));
        } else {
            attr_fns.push(Box::new(|scrollable: Scrollable| {
                scrollable.anchor_y(Anchor::End)
            }));
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
    inner
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic())
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

impl<'a>
    iced::widget::markdown::Viewer<
        'a,
        iced::widget::markdown::Uri,
        smudgy_theme::Theme,
        iced::Renderer,
    > for SmudgyMarkdownViewer
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
        let top = if index > 0 {
            settings.text_size.0 / 2.0
        } else {
            0.0
        };
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
        .style(
            move |_theme: &smudgy_theme::Theme| iced::widget::container::Style {
                background: Some(iced::Background::Color(panel_background)),
                border: iced::border::rounded(4),
                ..iced::widget::container::Style::default()
            },
        )
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
            let marker: iced::Element<
                'a,
                iced::widget::markdown::Uri,
                smudgy_theme::Theme,
                iced::Renderer,
            > = match bullet {
                Bullet::Point { .. } => iced::widget::text("\u{2022}")
                    .size(settings.text_size)
                    .color(body)
                    .into(),
                Bullet::Task { done, .. } => iced::Element::from(
                    iced::widget::container(iced::widget::checkbox(*done).size(settings.text_size))
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
        _ => format!(
            "\u{1f}auto\u{1f}{}",
            NEXT_TEXT_EDITOR_ID.fetch_add(1, Ordering::Relaxed)
        ),
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

    // Buffers are never removed from the store: an explicit-`id` buffer is
    // *reclaimed* (reseeded) by the next mount of that id, and an auto-keyed
    // buffer persists for the session. Reaping unmounted buffers the way map
    // entries are reaped must wait until `EditorHandle::element` no longer
    // lifts its `Content` borrow to `'static` (docs/widgets.md, "Widget
    // lifecycle") — freeing a borrowed buffer would turn that lift into a
    // use-after-free.

    // `value` is authoritative per mount. The build op runs on the session thread where the
    // UI-thread store isn't reachable, so we reseed on the FIRST frame of this build instead: a
    // fresh mount (e.g. a script reload that re-uses the same `id`) resets the buffer to `value`,
    // while later frames of the same mount preserve in-progress edits.
    let seeded = std::cell::Cell::new(false);

    Element::new(move || {
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
    })
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
            .style(
                move |_theme: &smudgy_theme::Theme| iced::widget::container::Style {
                    background: Some(iced::Background::Color(background)),
                    ..Default::default()
                },
            );
        let mut backdrop = iced::widget::mouse_area(backdrop);
        if let Some(on_dismiss) = &on_dismiss {
            backdrop = backdrop.on_press(WidgetMessage::InvokeCallback {
                callback: on_dismiss.clone(),
                isolate: isolate.clone(),
                args: Vec::new(),
            });
        }

        let layers: Vec<
            iced::Element<'static, WidgetMessage, smudgy_theme::Theme, iced::Renderer>,
        > = vec![
            iced::widget::opaque(backdrop),
            iced::widget::center(child.element()).into(),
        ];
        iced::widget::stack(layers).into()
    })
}

#[op2]
#[cppgc]
fn op_smudgy_widget_build_map_view(
    scope: &mut v8::PinScope,
    state: &mut OpState,
    props: v8::Local<v8::Object>,
) -> Element {
    let mapper = state.borrow::<Option<Mapper>>().clone();
    // View-global knobs are plain bindable scalars. The style surface is a
    // static named palette (`styles`) plus bindable structured props:
    // `defaultStyle`, the per-item `apply` associations (the dynamic hot
    // path), and semantic `doors` state.
    let room_spacing = get_dyn_f32_prop!(scope, state, props, "roomSpacing");
    let player_color = get_dyn_string_prop!(scope, state, props, "playerColor");
    let show_doors = get_dyn_bool_prop!(scope, state, props, "showDoors");
    let default_style = get_serde_prop::<MapStyleProp, smudgy_map_widget::MapStyle>(
        scope,
        state,
        props,
        "defaultStyle",
        map_style_from_node,
        Into::into,
    );
    let styles = get_static_styles(scope, props);
    let apply = get_serde_prop::<
        Vec<MapStyleApplicationProp>,
        Vec<smudgy_map_widget::MapStyleApplication>,
    >(
        scope,
        state,
        props,
        "apply",
        style_applications_from_node,
        convert_style_applications,
    );
    let doors = get_serde_prop::<Vec<MapDoorStateProp>, Vec<smudgy_map_widget::MapDoorState>>(
        scope,
        state,
        props,
        "doors",
        door_states_from_node,
        convert_door_states,
    );
    let widget_id = NEXT_MAP_WIDGET_ID.fetch_add(1, Ordering::Relaxed);

    // The reap guard rides inside the render closure: the closure is the only
    // path back into the session's `MapStore`, and its clones are exactly the
    // JS-held element handle plus any `WidgetRoot` mount. When the last clone
    // drops — unmount, engine-rebuild clear, or cppgc collecting the JS
    // handle, on whichever thread — the guard queues the id and the UI thread
    // frees the entry on its next render pass.
    let reap = state
        .borrow::<SmudgyWidgetRoot>()
        .map_reaper()
        .guard(widget_id);

    Element::new(move || {
        if let Some(mapper) = mapper.clone() {
            crate::map::with_active_store(|store| {
                let widget_id = reap.id();
                let presentation = smudgy_map_widget::MapViewPresentation {
                    room_spacing: room_spacing.as_ref().and_then(DynProp::get).unwrap_or(1.0),
                    player_color: player_color.as_ref().and_then(DynProp::get),
                    show_doors: show_doors.as_ref().and_then(DynProp::get).unwrap_or(true),
                    default_style: default_style
                        .as_ref()
                        .and_then(SerdeProp::get)
                        .unwrap_or_default(),
                    styles: styles.clone(),
                    apply: apply.as_ref().and_then(SerdeProp::get).unwrap_or_default(),
                    doors: doors.as_ref().and_then(SerdeProp::get).unwrap_or_default(),
                };
                let handle = store.ensure_map(mapper.clone(), widget_id, presentation);
                Some(
                    handle
                        .element()
                        .map(move |message| crate::WidgetMessage::MapMessage {
                            id: widget_id,
                            message,
                        }),
                )
            })
            .flatten()
            .unwrap_or_else(|| iced::widget::text("map unavailable").into())
        } else {
            iced::widget::text("map unavailable (no mapper)").into()
        }
    })
}

/// The `styles` palette: a static prop by design (`apply` entries change per
/// route step; the palette does not), read once at build. A binding token or
/// malformed record is reported and treated as an empty palette.
fn get_static_styles(
    scope: &mut v8::PinScope,
    props: v8::Local<v8::Object>,
) -> std::collections::HashMap<String, smudgy_map_widget::MapStyle> {
    let Some(key) = v8::String::new(scope, "styles") else {
        return std::collections::HashMap::new();
    };
    let Some(value) = props.get(scope, key.into()) else {
        return std::collections::HashMap::new();
    };
    if value.is_null_or_undefined() {
        return std::collections::HashMap::new();
    }
    match deno_core::serde_v8::from_v8::<std::collections::HashMap<String, MapStyleProp>>(
        scope, value,
    ) {
        Ok(styles) => styles
            .into_iter()
            .map(|(name, style)| (name, style.into()))
            .collect(),
        Err(err) => {
            warn_once(format!(
                "smudgy widgets: `styles` prop failed to parse (it must be a static record of \
                 named styles): {err}"
            ));
            std::collections::HashMap::new()
        }
    }
}

/// Per-item presentation channels, camelCase from JS. Absent fields inherit
/// `defaultStyle`, then the widget default.
#[derive(Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct MapStyleProp {
    room_fill: Option<String>,
    room_stroke: Option<String>,
    room_stroke_width: Option<f32>,
    room_border_radius: Option<f32>,
    connection_color: Option<String>,
    connection_width: Option<f32>,
    door_color: Option<String>,
    cross_area_label_visibility: Option<CrossAreaLabelVisibilityProp>,
    cross_area_label_background: Option<String>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum CrossAreaLabelVisibilityProp {
    Always,
    Hover,
    Never,
}

impl From<CrossAreaLabelVisibilityProp> for smudgy_map_widget::CrossAreaLabelVisibility {
    fn from(value: CrossAreaLabelVisibilityProp) -> Self {
        match value {
            CrossAreaLabelVisibilityProp::Always => Self::Always,
            CrossAreaLabelVisibilityProp::Hover => Self::Hover,
            CrossAreaLabelVisibilityProp::Never => Self::Never,
        }
    }
}

impl From<MapStyleProp> for smudgy_map_widget::MapStyle {
    fn from(value: MapStyleProp) -> Self {
        Self {
            room_fill: value.room_fill,
            room_stroke: value.room_stroke,
            room_stroke_width: value.room_stroke_width,
            room_border_radius: value.room_border_radius,
            connection_color: value.connection_color,
            connection_width: value.connection_width,
            door_color: value.door_color,
            cross_area_label_visibility: value.cross_area_label_visibility.map(Into::into),
            cross_area_label_background: value.cross_area_label_background,
        }
    }
}

fn map_style_from_node(node: &Node) -> Result<smudgy_map_widget::MapStyle, serde_json::Error> {
    serde_from_node::<MapStyleProp>(node).map(Into::into)
}

/// One Connection selected from either endpoint by room + direction (never a
/// ConnectionId: its u64 halves exceed `Number.MAX_SAFE_INTEGER` and cannot
/// travel the JSON store-binding path).
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MapExitRefProp {
    room: i32,
    direction: smudgy_cloud::ExitDirection,
}

impl From<MapExitRefProp> for smudgy_map_widget::MapExitRef {
    fn from(value: MapExitRefProp) -> Self {
        Self {
            room: smudgy_cloud::RoomNumber(value.room),
            direction: value.direction,
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MapStyleApplicationProp {
    style: String,
    #[serde(default)]
    rooms: Vec<i32>,
    #[serde(default)]
    exits: Vec<MapExitRefProp>,
    /// Area scope in either accepted spelling (see [`MapAreaIdProp`]);
    /// entries scoped to another area are ignored at resolution.
    #[serde(default)]
    area: Option<MapAreaIdProp>,
}

/// An apply entry's `area` scope in either accepted spelling: the `[hi, lo]`
/// u64 id halves (BigInt-carried on the static prop path) or the canonical
/// hyphenated UUID string. The string is the JSON-safe spelling: real id
/// halves exceed `Number.MAX_SAFE_INTEGER` and surface as `BigInt`, which
/// `JSON.stringify` rejects, so store-bound apply arrays carry the string.
#[derive(Clone)]
enum MapAreaIdProp {
    Pair(u64, u64),
    Text(String),
}

impl<'de> Deserialize<'de> for MapAreaIdProp {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct AreaIdVisitor;

        impl<'de> serde::de::Visitor<'de> for AreaIdVisitor {
            type Value = MapAreaIdProp;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("an `[hi, lo]` area id pair or a UUID string")
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(MapAreaIdProp::Text(value.to_owned()))
            }

            fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
                Ok(MapAreaIdProp::Text(value))
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                let hi = seq
                    .next_element::<u64>()?
                    .ok_or_else(|| serde::de::Error::invalid_length(0, &self))?;
                let lo = seq
                    .next_element::<u64>()?
                    .ok_or_else(|| serde::de::Error::invalid_length(1, &self))?;
                if seq.next_element::<serde::de::IgnoredAny>()?.is_some() {
                    return Err(serde::de::Error::invalid_length(3, &self));
                }
                Ok(MapAreaIdProp::Pair(hi, lo))
            }
        }

        deserializer.deserialize_any(AreaIdVisitor)
    }
}

impl MapAreaIdProp {
    /// Resolve either spelling to the internal id. A string that is not a
    /// UUID reports once and yields `None`; the caller drops that entry —
    /// an entry whose scope cannot be resolved must not widen to every area.
    fn resolve(&self) -> Option<smudgy_cloud::AreaId> {
        match self {
            Self::Pair(hi, lo) => Some(smudgy_cloud::AreaId(smudgy_cloud::Uuid::from_u64_pair(
                *hi, *lo,
            ))),
            Self::Text(text) => match text.parse::<smudgy_cloud::Uuid>() {
                Ok(uuid) => Some(smudgy_cloud::AreaId(uuid)),
                Err(_) => {
                    warn_once(format!(
                        "smudgy widgets: apply entry `area` {text:?} is not a UUID; entry skipped"
                    ));
                    None
                }
            },
        }
    }
}

impl MapStyleApplicationProp {
    /// Convert to the widget-side entry; `None` when the `area` scope fails
    /// to resolve.
    fn resolve(self) -> Option<smudgy_map_widget::MapStyleApplication> {
        let area = match &self.area {
            None => None,
            Some(area) => Some(area.resolve()?),
        };
        Some(smudgy_map_widget::MapStyleApplication {
            style: self.style,
            rooms: self
                .rooms
                .into_iter()
                .map(smudgy_cloud::RoomNumber)
                .collect(),
            exits: self.exits.into_iter().map(Into::into).collect(),
            area,
        })
    }
}

fn style_applications_from_node(
    node: &Node,
) -> Result<Vec<smudgy_map_widget::MapStyleApplication>, serde_json::Error> {
    serde_from_node::<Vec<MapStyleApplicationProp>>(node).map(convert_style_applications)
}

fn convert_style_applications(
    entries: Vec<MapStyleApplicationProp>,
) -> Vec<smudgy_map_widget::MapStyleApplication> {
    entries
        .into_iter()
        .filter_map(MapStyleApplicationProp::resolve)
        .collect()
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MapDoorStateProp {
    exit: MapExitRefProp,
    #[serde(default)]
    closed: Option<bool>,
    #[serde(default)]
    locked: Option<bool>,
}

impl From<MapDoorStateProp> for smudgy_map_widget::MapDoorState {
    fn from(value: MapDoorStateProp) -> Self {
        Self {
            exit: value.exit.into(),
            closed: value.closed,
            locked: value.locked,
        }
    }
}

fn door_states_from_node(
    node: &Node,
) -> Result<Vec<smudgy_map_widget::MapDoorState>, serde_json::Error> {
    serde_from_node::<Vec<MapDoorStateProp>>(node).map(convert_door_states)
}

fn convert_door_states(entries: Vec<MapDoorStateProp>) -> Vec<smudgy_map_widget::MapDoorState> {
    entries.into_iter().map(Into::into).collect()
}

/// Parse an author-written scene (a static `scene` value or a binding fallback) with
/// image-src resolution routed through the per-isolate registry memo — per-frame
/// `createWidget` re-parses of the same scene then never re-run URL parsing or `data:`
/// payload hashing (the memo's whole purpose). The registry `RefCell` is re-borrowed per
/// src, never held across reentrancy.
fn parse_scene_memoized(
    state: &OpState,
    image_store: &Option<ImageStore>,
    creator_token: u32,
    node: &Node,
) -> Result<crate::canvas::ParsedScene, crate::canvas::SceneReject> {
    let registry = state.borrow::<RefCell<ImageRegistry>>();
    let mut resolve = |src: &str| match image_store {
        Some(store) => registry
            .borrow_mut()
            .ensure_image_cell(creator_token, src, store),
        None => crate::canvas::ImageResolution::Rejected(
            "no image store is available in this runtime".to_string(),
        ),
    };
    let mut images = crate::canvas::SceneImages::Memoized(&mut resolve);
    crate::canvas::parse_scene(node, Some(&mut images))
}

#[op2]
#[cppgc]
fn op_smudgy_widget_build_canvas(
    scope: &mut v8::PinScope,
    state: &mut OpState,
    props: v8::Local<v8::Object>,
    #[string] isolate_token: &str,
    creator_token: u32,
) -> Element {
    use crate::canvas::{
        ParsedScene, PointerHandler, SceneImageCtx, SceneMemo, SceneProgram, SceneSource,
    };

    let width = get_dyn_length_prop!(scope, state, props, "width");
    let height = get_dyn_length_prop!(scope, state, props, "height");

    // Image resolution for `image` records (plan D7). Two provenances:
    // - Static scenes and binding fallbacks (both written by the widget's author) resolve
    //   through the per-isolate `ImageRegistry` memo, so per-frame `createWidget` re-parses
    //   of the same scene never re-run URL parsing or `data:` hashing.
    // - Live bound values re-parse on the UI thread (no OpState there) with the ctx below:
    //   bound provenance — descend-only srcs, no absolute paths (plan D2).
    let image_store = state
        .try_borrow::<Option<ImageStore>>()
        .and_then(Clone::clone);
    let bound_image_ctx = match &image_store {
        Some(store) => state
            .borrow::<RefCell<ImageRegistry>>()
            .borrow()
            .creator(creator_token)
            .map(|creator| SceneImageCtx {
                creator: creator.clone(),
                store: store.clone(),
                bound: true,
            }),
        None => None,
    };

    // `view_box: [x, y, w, h]` in scene units — frozen as an exact rect-to-bounds mapping
    // (non-uniform when aspects differ), so the scene<->pointer transform stays bijective.
    let view_box = {
        let key = ascii_str!("view_box")
            .v8_string(scope)
            .expect("Could not allocate string")
            .into();
        props
            .get(scope, key)
            .and_then(|v| deno_core::serde_v8::from_v8::<[f32; 4]>(scope, v).ok())
            .filter(|[_, _, w, h]| w.is_finite() && h.is_finite() && *w > 0.0 && *h > 0.0)
            .map(|[x, y, w, h]| iced::Rectangle::new(iced::Point::new(x, y), iced::Size::new(w, h)))
    };

    // The scene: a store binding (the live path — repaints per store flush, parse memoized by
    // snapshot pointer) or a static value converted to the store's `Node` shape once so both
    // sources share one parser. A rejected *static* scene has no previous generation to keep,
    // so it renders empty — loudly.
    let scene_key = ascii_str!("scene")
        .v8_string(scope)
        .expect("Could not allocate string")
        .into();
    let scene_value = props.get(scope, scene_key);
    let scene = match scene_value {
        Some(value) if is_binding_token(scope, value) => {
            match bound_prop_from_v8(scope, state, value) {
                Some(bound) => {
                    let fallback = bound
                    .fallback
                    .as_ref()
                    .and_then(|node| {
                        // The fallback is author-written (part of the binding declaration
                        // in the widget's own source): static provenance, memoized.
                        let parsed = parse_scene_memoized(
                            state,
                            &image_store,
                            creator_token,
                            node,
                        );
                        match parsed {
                            Ok(parsed) => {
                                crate::canvas::log_warnings(&parsed);
                                Some(parsed)
                            }
                            Err(reject) => {
                                log::warn!(
                                    "smudgy canvas: binding fallback rejected ({reject}); using an empty fallback"
                                );
                                None
                            }
                        }
                    })
                    .unwrap_or_default();
                    SceneSource::Bound {
                        cell: bound.cell,
                        memo: Arc::new(std::sync::Mutex::new(SceneMemo::default())),
                        fallback: Arc::new(fallback),
                        // Live bound values re-parse on the UI thread with bound provenance:
                        // descend-only srcs, no absolute paths (the producer of a bound scene
                        // is not the widget's author — plan D2).
                        image_ctx: bound_image_ctx.map(Arc::new),
                    }
                }
                None => SceneSource::Static(Arc::new(ParsedScene::default())),
            }
        }
        Some(value) if !value.is_null_or_undefined() => {
            let parsed = deno_core::serde_v8::from_v8::<serde_json::Value>(scope, value)
                .ok()
                .map_or_else(ParsedScene::default, |value| {
                    let parsed = parse_scene_memoized(
                        state,
                        &image_store,
                        creator_token,
                        &Node::from(value),
                    );
                    match parsed {
                        Ok(parsed) => {
                            crate::canvas::log_warnings(&parsed);
                            parsed
                        }
                        Err(reject) => {
                            log::warn!(
                                "smudgy canvas: static scene rejected ({reject}); rendering nothing"
                            );
                            ParsedScene::default()
                        }
                    }
                });
            SceneSource::Static(Arc::new(parsed))
        }
        _ => SceneSource::Static(Arc::new(ParsedScene::default())),
    };

    let on_pointer =
        get_v8_function_prop!(scope, props, "onPointer").map(|callback| PointerHandler {
            callback: Arc::new(callback),
            isolate: WidgetIsolate(isolate_token.to_string()),
        });

    // `fit: "contain"` opts into uniform scale-to-fit with centering; the default is the
    // exact (possibly non-uniform) rect-to-bounds mapping.
    let fit = match get_opt_string_prop!(scope, props, "fit").as_deref() {
        Some("contain") => crate::canvas::ViewFit::Contain,
        _ => crate::canvas::ViewFit::Fill,
    };

    let program = SceneProgram {
        scene,
        view_box,
        fit,
        on_pointer,
        image_store,
    };

    Element::new(move || {
        let width = width
            .as_ref()
            .and_then(DynProp::get)
            .unwrap_or(iced::Length::Fill);
        let height = height
            .as_ref()
            .and_then(DynProp::get)
            .unwrap_or(iced::Length::Fill);
        // Clipped like the map canvas: scene geometry may exceed the bounds (the burst-alert
        // ring deliberately does), and tiny-skia's damage-tracked partial redraws would leave
        // the spill on screen without the clipping container.
        iced::widget::container(
            iced::widget::canvas(program.clone())
                .width(width)
                .height(height),
        )
        .width(width)
        .height(height)
        .clip(true)
        .into()
    })
}

// ---- Image widget (plan D6) --------------------------------------------------------------
//
// `<Image src=... />` maps to `iced::widget::image`. The build op stays I/O-free: it resolves
// + policy-checks the `src` (lexically, via `smudgy_cloud::image_source`) and `ensure`s the
// process-global `ImageStore`, capturing the returned entry cell into the render closure. Per
// frame the closure does one lock-free `cell.load()`; the ui-side fetcher performs all I/O.

/// Per-isolate image state parked in `OpState`: validated creators (indexed by their 1-based
/// token) and a bounded resolve memo so per-frame `createWidget` doesn't re-parse a `src`
/// every frame. Fresh per isolate — a token from one isolate is meaningless in another.
#[derive(Default)]
struct ImageRegistry {
    /// 1-based tokens: `creators[token - 1]`. `None` = a registration that failed membership
    /// (forged/denied creator); its token resolves every `src` to the broken state.
    creators: Vec<Option<Arc<RegisteredImageCreator>>>,
    /// Per-creator bound-src resolve tables (plan D3's side-table), parallel to `creators`.
    bound_tables: Vec<Arc<BoundSrcTable>>,
    /// `(token, src) -> resolution + precomputed store key`. Bounded by entry count AND by
    /// retained bytes (a `data:` source keeps its whole URI alive in the resolution —
    /// count alone would let 4096 × 2 MiB URIs sit resident); either cap clears wholesale
    /// (the hot path is a small set of distinct srcs; a flood just re-parses, never leaks).
    memo: std::collections::HashMap<(u32, SrcMemoKey), Result<MemoHit, String>>,
    /// Bytes retained by `memo` values (dominated by `data:` URI payloads).
    memo_bytes: usize,
}

/// A memoized successful resolution: the source plus its store cache key, computed once —
/// the per-frame path hands both to [`ImageStore::ensure_keyed`] and never re-derives the
/// key (for a `data:` src that derivation once SHA-256'd megabytes per frame).
struct MemoHit {
    source: ResolvedImageSource,
    key: Arc<str>,
}

impl MemoHit {
    /// Bytes this entry keeps alive (memo cap accounting).
    fn retained_bytes(&self) -> usize {
        retained_source_bytes(&self.source) + self.key.len()
    }
}

/// Bytes a retained [`ResolvedImageSource`] keeps alive (`data:` URIs dominate).
fn retained_source_bytes(source: &ResolvedImageSource) -> usize {
    match source {
        ResolvedImageSource::Data { uri, .. } => uri.len(),
        ResolvedImageSource::Remote(url) => url.as_str().len(),
        ResolvedImageSource::LocalFile(path) => path.as_os_str().len(),
        ResolvedImageSource::PackageAsset {
            owner,
            name,
            version,
            subpath,
        } => owner.len() + name.len() + version.len() + subpath.len(),
    }
}

/// Upper bounds on the resolve memo before a wholesale clear: entry count and retained
/// bytes. Generous vs any realistic distinct-`src` count in one isolate; they exist only to
/// bound a pathological flood.
const IMAGE_MEMO_CAP: usize = 4096;
const IMAGE_MEMO_BYTE_CAP: usize = 32 * 1024 * 1024;
/// Registrations are one-per-`makeWidgets`-instance; anything near this is a script
/// looping the op directly. Past it, registration returns the denied token.
const IMAGE_CREATOR_CAP: usize = 4096;

impl ImageRegistry {
    fn register(&mut self, creator: Option<RegisteredImageCreator>) -> u32 {
        if self.creators.len() >= IMAGE_CREATOR_CAP {
            return 0;
        }
        self.creators.push(creator.map(Arc::new));
        self.bound_tables.push(Arc::new(BoundSrcTable::default()));
        u32::try_from(self.creators.len()).unwrap_or(0)
    }

    fn creator(&self, token: u32) -> Option<&Arc<RegisteredImageCreator>> {
        if token == 0 {
            return None;
        }
        self.creators
            .get((token - 1) as usize)
            .and_then(Option::as_ref)
    }

    fn bound_table(&self, token: u32) -> Option<Arc<BoundSrcTable>> {
        if token == 0 {
            return None;
        }
        self.bound_tables.get((token - 1) as usize).cloned()
    }

    /// Ensure the memo holds a resolution for `(token, raw)` (static provenance),
    /// spawning the store fetch on first sight; returns the memo key for a follow-up
    /// shared-borrow read. Steady state per frame: one bounded memo-key hash + a map
    /// probe — no URL parse, no payload hashing.
    fn resolve_memoized(&mut self, token: u32, raw: &str, store: &ImageStore) -> (u32, SrcMemoKey) {
        let key = (token, memo_key(raw));
        if self.memo.contains_key(&key) {
            return key;
        }
        let outcome = match self.creator(token) {
            Some(creator) => match resolve_src(raw, creator, false) {
                Ok(source) => {
                    let store_key: Arc<str> = Arc::from(source.store_key(&creator.policy));
                    let _ = store.ensure_keyed(&store_key, &source, &creator.policy);
                    Ok(MemoHit {
                        source,
                        key: store_key,
                    })
                }
                Err(e) => Err(e.to_string()),
            },
            None => Err("image creator was not registered or was denied".to_string()),
        };
        if let Err(reason) = &outcome {
            log::warn!("smudgy images: '{}' rejected: {reason}", LogSrc(raw));
        }
        if self.memo.len() >= IMAGE_MEMO_CAP || self.memo_bytes >= IMAGE_MEMO_BYTE_CAP {
            self.memo.clear();
            self.memo_bytes = 0;
        }
        if let Ok(hit) = &outcome {
            self.memo_bytes += hit.retained_bytes();
        }
        self.memo.insert(key.clone(), outcome);
        key
    }

    /// Resolve a static `raw` for `token` and ensure its store entry, memoized by
    /// `(token, memo_key)`. Steady state per frame: memo hit + `ensure_keyed` (borrowed
    /// store lookup + relaxed recency touch) — no URL parse, no payload hashing, no
    /// allocation. Bound values never come through here — they resolve in the render
    /// closure via the creator's [`BoundSrcTable`].
    fn ensure_static(
        &mut self,
        token: u32,
        raw: &str,
        store: &ImageStore,
    ) -> Option<Arc<ImageEntryCell>> {
        let key = self.resolve_memoized(token, raw, store);
        match (self.memo.get(&key), self.creator(token)) {
            (Some(Ok(hit)), Some(creator)) => {
                Some(store.ensure_keyed(&hit.key, &hit.source, &creator.policy))
            }
            _ => None,
        }
    }

    /// The canvas variant of [`ensure_static`](Self::ensure_static): same memo, but the
    /// canvas retains the resolution inputs in a [`crate::canvas::ImageCell`] so its
    /// per-redraw refresh walk can keep the slot LRU-hot and revive evicted cells. The
    /// per-parse `Arc`/clone construction is cold relative to the scene parse around it.
    fn ensure_image_cell(
        &mut self,
        token: u32,
        raw: &str,
        store: &ImageStore,
    ) -> crate::canvas::ImageResolution {
        use crate::canvas::{ImageCell, ImageResolution};
        let key = self.resolve_memoized(token, raw, store);
        match (self.memo.get(&key), self.creator(token)) {
            (Some(Ok(hit)), Some(creator)) => {
                let cell = store.ensure_keyed(&hit.key, &hit.source, &creator.policy);
                ImageResolution::Resolved(Arc::new(ImageCell::new(
                    hit.key.clone(),
                    hit.source.clone(),
                    creator.policy.clone(),
                    cell,
                )))
            }
            (Some(Err(reason)), _) => ImageResolution::Rejected(reason.clone()),
            _ => ImageResolution::Rejected(
                "image creator was not registered or was denied".to_string(),
            ),
        }
    }
}

/// A `src` for log lines: `data:` URIs are megabytes — never print one whole.
struct LogSrc<'a>(&'a str);

impl std::fmt::Display for LogSrc<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        const MAX: usize = 120;
        if self.0.len() <= MAX {
            f.write_str(self.0)
        } else {
            let cut = (0..=MAX)
                .rev()
                .find(|i| self.0.is_char_boundary(*i))
                .unwrap_or(0);
            write!(f, "{}… ({} bytes)", &self.0[..cut], self.0.len())
        }
    }
}

/// Register (once, in `makeWidgets`) the calling creator for image-src resolution. Validates
/// the JS-supplied descriptor against this isolate's [`ImageSourcePolicy`] and returns a
/// `u32` token the `<Image>` build op passes back — so a forged `__creator` cannot select
/// another package's asset root (a denied registration yields a token whose srcs all break).
/// `module` is the importing module's in-package path (the `?mod=` value), `""` for none.
#[op2(fast)]
fn op_smudgy_widget_register_image_creator(
    state: &mut OpState,
    #[string] creator_json: &str,
    #[string] module: &str,
) -> u32 {
    let policy = state
        .try_borrow::<ImageSourcePolicy>()
        .map(|p| Arc::new(p.clone()));
    let module = (!module.is_empty()).then_some(module);
    let creator = policy.and_then(|policy| register_creator(creator_json, module, policy));
    state
        .borrow::<RefCell<ImageRegistry>>()
        .borrow_mut()
        .register(creator)
}

/// Content-fit mapping (`iced_core::ContentFit`); default `Contain` (the widget default).
fn content_fit_from(value: Option<&str>) -> iced::ContentFit {
    match value {
        Some("cover") => iced::ContentFit::Cover,
        Some("fill") => iced::ContentFit::Fill,
        Some("none") => iced::ContentFit::None,
        Some("scale-down") => iced::ContentFit::ScaleDown,
        _ => iced::ContentFit::Contain,
    }
}

fn filter_method_from(value: Option<&str>) -> iced::widget::image::FilterMethod {
    match value {
        Some("nearest") => iced::widget::image::FilterMethod::Nearest,
        _ => iced::widget::image::FilterMethod::Linear,
    }
}

/// Build the iced element for a resolved entry cell: the decoded image when `Ready`, else a
/// sized placeholder honoring `width`/`height` (so an explicit box reserves space while the
/// load is in flight or after a failure).
fn image_element_from_cell(
    cell: Option<&Arc<ImageEntryCell>>,
    width: Option<iced::Length>,
    height: Option<iced::Length>,
    content_fit: iced::ContentFit,
    filter: iced::widget::image::FilterMethod,
    opacity: f32,
    rotation_deg: f32,
) -> iced::Element<'static, WidgetMessage, smudgy_theme::Theme, iced::Renderer> {
    if let Some(cell) = cell
        && let EntryState::Ready { handle, .. } = &*cell.state()
    {
        let mut image = iced::widget::image(handle.clone())
            .content_fit(content_fit)
            .filter_method(filter)
            .opacity(opacity)
            .rotation(iced::Radians(rotation_deg.to_radians()));
        if let Some(width) = width {
            image = image.width(width);
        }
        if let Some(height) = height {
            image = image.height(height);
        }
        return image.into();
    }
    // Loading / Failed / unresolved: a placeholder that reserves explicit dimensions.
    let mut space = iced::widget::Space::new();
    if let Some(width) = width {
        space = space.width(width);
    }
    if let Some(height) = height {
        space = space.height(height);
    }
    space.into()
}

#[op2]
#[cppgc]
fn op_smudgy_widget_build_image(
    scope: &mut v8::PinScope,
    state: &mut OpState,
    props: v8::Local<v8::Object>,
    creator_token: u32,
) -> Element {
    let width = get_dyn_length_prop!(scope, state, props, "width");
    let height = get_dyn_length_prop!(scope, state, props, "height");
    let opacity = get_dyn_f32_prop!(scope, state, props, "opacity");
    let content_fit =
        content_fit_from(get_opt_string_prop!(scope, props, "content_fit").as_deref());
    let filter = filter_method_from(get_opt_string_prop!(scope, props, "filter_method").as_deref());
    // Degrees fit losslessly in f32 for any sane rotation; truncation is fine.
    #[allow(clippy::cast_possible_truncation)]
    let rotation_deg = get_number_prop!(scope, props, "rotation").unwrap_or(0.0) as f32;

    let store = state
        .try_borrow::<Option<ImageStore>>()
        .and_then(Clone::clone);

    // `src` is either a static string or a store binding (its value changes per frame).
    let src = get_dyn_string_prop!(scope, state, props, "src");

    // Resolve a static src once, here; capture the entry cell. A bound src is resolved in the
    // render closure (its string is only known per frame).
    let static_cell = match (&src, &store) {
        (Some(DynProp::Static(raw)), Some(store)) => state
            .borrow::<RefCell<ImageRegistry>>()
            .borrow_mut()
            .ensure_static(creator_token, raw, store),
        _ => None,
    };

    // For a bound src, capture what the render closure needs to resolve per distinct value.
    let bound_ctx = match (&src, &store) {
        (Some(DynProp::Bound { .. }), Some(store)) => {
            let registry = state.borrow::<RefCell<ImageRegistry>>();
            let registry = registry.borrow();
            match (
                registry.creator(creator_token),
                registry.bound_table(creator_token),
            ) {
                (Some(creator), Some(table)) => Some(BoundImageCtx {
                    creator: creator.clone(),
                    store: store.clone(),
                    table,
                    memo: RefCell::new(None),
                }),
                _ => None,
            }
        }
        _ => None,
    };

    let default_opacity = opacity.as_ref().and_then(DynProp::get).unwrap_or(1.0);

    Element::new(move || {
        let w = width.as_ref().and_then(DynProp::get);
        let h = height.as_ref().and_then(DynProp::get);
        let op = opacity
            .as_ref()
            .and_then(DynProp::get)
            .unwrap_or(default_opacity);
        // Resolve the live cell: static (captured once) or bound (per distinct value).
        let cell = match (&static_cell, &bound_ctx, &src) {
            (Some(cell), _, _) => Some(cell.clone()),
            (None, Some(ctx), Some(DynProp::Bound { prop, .. })) => {
                ctx.resolve(&prop.display_text())
            }
            _ => None,
        };
        image_element_from_cell(cell.as_ref(), w, h, content_fit, filter, op, rotation_deg)
    })
}

/// The largest bound `src` the side-table will key by. Above this, per-frame table hashing
/// of the raw string would itself blow the budget — oversized values (multi-KiB `data:`
/// URIs fed through a binding) re-resolve per rebuild instead, and the `.d.ts` tells
/// authors to hoist them.
const BOUND_TABLE_MAX_SRC: usize = 4096;
/// Side-table caps (entries / retained bytes) before a wholesale clear.
const BOUND_TABLE_CAP: usize = 256;
const BOUND_TABLE_BYTE_CAP: usize = 1024 * 1024;

/// Plan D3's bound-src side-table, one per registered creator: `raw value → resolution`,
/// read lock-free on the UI thread. Per-frame `createWidget` rebuilds give the render
/// closure (and thus [`BoundImageCtx`]'s local memo) a one-frame lifetime — without this
/// table every rebuild would URL-parse + policy-check the bound value on the UI thread.
/// Failures memoize too (`Err`), so a steadily-invalid value costs one lookup per frame and
/// warns exactly once, at insert.
#[derive(Default)]
struct BoundSrcTable {
    map: arc_swap::ArcSwap<std::collections::HashMap<Box<str>, BoundResolution>>,
    /// Guards insertions (COW map swap) and tracks retained bytes.
    writer: std::sync::Mutex<usize>,
}

type BoundResolution = Result<(ResolvedImageSource, Arc<str>), ()>;

impl BoundSrcTable {
    /// Resolve `raw` as a bound value for `creator`, ensuring the store entry. Steady
    /// state: one lock-free map load + hash of `raw` (≤ [`BOUND_TABLE_MAX_SRC`] bytes) +
    /// `ensure_keyed`. First sight of a distinct value takes the writer mutex once.
    fn resolve(
        &self,
        raw: &str,
        creator: &RegisteredImageCreator,
        store: &ImageStore,
    ) -> Option<Arc<ImageEntryCell>> {
        if raw.len() > BOUND_TABLE_MAX_SRC {
            // Too big to key by: resolve fresh (pathological, documented in the .d.ts).
            let source = resolve_src(raw, creator, true).ok()?;
            return Some(store.ensure(&source, &creator.policy));
        }
        if let Some(resolution) = self.map.load().get(raw) {
            return match resolution {
                Ok((source, key)) => Some(store.ensure_keyed(key, source, &creator.policy)),
                Err(()) => None,
            };
        }
        // First sight of this value: resolve under the writer lock and publish.
        let mut bytes = self.writer.lock().expect("bound table writer");
        if let Some(resolution) = self.map.load().get(raw) {
            // Lost the insert race; use the winner's entry.
            return match resolution {
                Ok((source, key)) => Some(store.ensure_keyed(key, source, &creator.policy)),
                Err(()) => None,
            };
        }
        // A bound value is treated as `bound = true` (descend-only, no absolute paths —
        // a producer is not the widget's author).
        let (resolution, cell) = match resolve_src(raw, creator, true) {
            Ok(source) => {
                let key: Arc<str> = Arc::from(source.store_key(&creator.policy));
                let cell = store.ensure_keyed(&key, &source, &creator.policy);
                (Ok((source, key)), Some(cell))
            }
            Err(reason) => {
                log::warn!(
                    "smudgy images: bound src '{}' rejected: {reason}",
                    LogSrc(raw)
                );
                (Err(()), None)
            }
        };
        let mut map = (**self.map.load()).clone();
        if map.len() >= BOUND_TABLE_CAP || *bytes >= BOUND_TABLE_BYTE_CAP {
            map.clear();
            *bytes = 0;
        }
        *bytes += raw.len()
            + resolution
                .as_ref()
                .map_or(0, |(source, key)| retained_source_bytes(source) + key.len());
        map.insert(Box::from(raw), resolution);
        self.map.store(Arc::new(map));
        cell
    }
}

/// The render-closure state for a store-bound `src`: a one-frame local memo (steady value =
/// one string compare, including memoized *failures*) over the creator's cross-frame
/// [`BoundSrcTable`].
struct BoundImageCtx {
    creator: Arc<RegisteredImageCreator>,
    store: ImageStore,
    table: Arc<BoundSrcTable>,
    memo: RefCell<Option<(String, Option<Arc<ImageEntryCell>>)>>,
}

impl BoundImageCtx {
    fn resolve(&self, raw: &str) -> Option<Arc<ImageEntryCell>> {
        {
            let memo = self.memo.borrow();
            if let Some((last, cell)) = memo.as_ref()
                && last == raw
            {
                match cell {
                    // Memoized failure: placeholder without re-resolving.
                    None => return None,
                    Some(cell) if !cell.is_evicted() => return Some(cell.clone()),
                    // Evicted mid-display: fall through to re-ensure.
                    Some(_) => {}
                }
            }
        }
        let cell = self.table.resolve(raw, &self.creator, &self.store);
        *self.memo.borrow_mut() = Some((raw.to_string(), cell.clone()));
        cell
    }
}

#[op2]
#[cppgc]
fn op_smudgy_widget_build_space(
    scope: &mut v8::PinScope,
    state: &mut OpState,
    props: v8::Local<v8::Object>,
) -> Element {
    let width = get_dyn_length_prop!(scope, state, props, "width");
    let height = get_dyn_length_prop!(scope, state, props, "height");
    Element::new(move || {
        let mut space = iced::widget::Space::new();
        if let Some(width) = width.as_ref().and_then(DynProp::get) {
            space = space.width(width);
        }
        if let Some(height) = height.as_ref().and_then(DynProp::get) {
            space = space.height(height);
        }
        space.into()
    })
}

type Checkbox = iced::widget::Checkbox<'static, WidgetMessage, smudgy_theme::Theme, iced::Renderer>;

#[op2]
#[cppgc]
fn op_smudgy_widget_build_checkbox(
    scope: &mut v8::PinScope,
    state: &mut OpState,
    props: v8::Local<v8::Object>,
    parts: v8::Local<v8::Array>,
    #[string] isolate_token: &str,
) -> Element {
    let checked = get_dyn_bool_prop!(scope, state, props, "checked");
    let size = get_dyn_f32_prop!(scope, state, props, "size");
    let text_size = get_dyn_f32_prop!(scope, state, props, "text_size");
    let label = TextContent::collect(scope, state, parts);
    // No `onToggle` leaves iced's `on_toggle` unset, which renders the disabled style — the
    // right read for a display-only checkmark (unlike Radio, whose factory requires a
    // handler, because iced has no disabled radio rendering).
    let on_toggle = get_v8_function_prop!(scope, props, "onToggle").map(Arc::new);
    let isolate = WidgetIsolate(isolate_token.to_string());

    Element::new(move || {
        let mut checkbox: Checkbox =
            iced::widget::checkbox(checked.as_ref().and_then(DynProp::get).unwrap_or(false));
        if label.has_parts() {
            checkbox = checkbox.label(label.current());
        }
        if let Some(on_toggle) = &on_toggle {
            let callback = on_toggle.clone();
            let isolate = isolate.clone();
            checkbox = checkbox.on_toggle(move |now_checked| WidgetMessage::InvokeCallback {
                callback: callback.clone(),
                isolate: isolate.clone(),
                args: vec![now_checked.to_string()],
            });
        }
        if let Some(size) = size.as_ref().and_then(DynProp::get) {
            checkbox = checkbox.size(size);
        }
        if let Some(text_size) = text_size.as_ref().and_then(DynProp::get) {
            checkbox = checkbox.text_size(text_size);
        }
        checkbox.into()
    })
}

type Radio = iced::widget::Radio<'static, WidgetMessage, smudgy_theme::Theme, iced::Renderer>;
type Tooltip = iced::widget::Tooltip<'static, WidgetMessage, smudgy_theme::Theme, iced::Renderer>;

#[op2]
#[cppgc]
fn op_smudgy_widget_build_tooltip(
    scope: &mut v8::PinScope,
    state: &mut OpState,
    props: v8::Local<v8::Object>,
    #[cppgc] target: &Element,
    #[cppgc] tip: &Element,
) -> Element {
    use iced::widget::tooltip::Position;

    let target = target.clone();
    let tip = tip.clone();
    let position = match get_string_prop!(scope, props, "position").as_deref() {
        Some("bottom") => Position::Bottom,
        Some("left") => Position::Left,
        Some("right") => Position::Right,
        Some("cursor") => Position::FollowCursor,
        _ => Position::Top,
    };
    let gap = get_dyn_f32_prop!(scope, state, props, "gap");
    // Set by the factory for string/binding tips: those get the themed chrome (surface +
    // border + padding) from the tooltip's own container styling — never an extra wrapper
    // element, whose padding and background would stack with the tooltip's. Element tips
    // render chrome-free; styling them is the author's element's job.
    let chrome = get_bool_prop!(scope, props, "tip_chrome").unwrap_or(false);

    Element::new(move || {
        let mut tooltip: Tooltip = iced::widget::tooltip(target.element(), tip.element(), position);
        if let Some(gap) = gap.as_ref().and_then(DynProp::get) {
            tooltip = tooltip.gap(gap);
        }
        if chrome {
            tooltip = tooltip
                .padding(6.0)
                .style(smudgy_theme::builtins::container::tooltip);
        }
        tooltip.into()
    })
}

#[op2]
#[cppgc]
fn op_smudgy_widget_build_radio(
    scope: &mut v8::PinScope,
    state: &mut OpState,
    props: v8::Local<v8::Object>,
    parts: v8::Local<v8::Array>,
    #[string] isolate_token: &str,
) -> Element {
    let value = get_string_prop!(scope, props, "value").unwrap_or_default();
    let selected = get_dyn_string_prop!(scope, state, props, "selected");
    let size = get_dyn_f32_prop!(scope, state, props, "size");
    let text_size = get_dyn_f32_prop!(scope, state, props, "text_size");
    let label = TextContent::collect(scope, state, parts);
    // The factory requires `onSelect` (a handler-less radio would render enabled and swallow
    // clicks); the op stays defensive with a Noop for direct op callers. The click message
    // depends only on build-time values, so it is built once here and cloned per frame.
    let on_select = get_v8_function_prop!(scope, props, "onSelect").map(Arc::new);
    let isolate = WidgetIsolate(isolate_token.to_string());
    let message = match on_select {
        Some(callback) => WidgetMessage::InvokeCallback {
            callback,
            isolate,
            args: vec![value.clone()],
        },
        None => WidgetMessage::Noop,
    };

    Element::new(move || {
        // iced's radio wants `V: Copy + Eq`, which the script-level string value is not;
        // the string comparison happens here and a `bool` adapter drives iced. The label
        // rides unconditionally — unlike Checkbox, iced's radio has no label-less form,
        // so authored-empty content renders as an empty text run.
        let is_selected = selected
            .as_ref()
            .and_then(DynProp::get)
            .is_some_and(|current| current == value);
        let message = message.clone();
        let mut radio: Radio = iced::widget::radio(
            label.current(),
            true,
            is_selected.then_some(true),
            move |_| message,
        );
        if let Some(size) = size.as_ref().and_then(DynProp::get) {
            radio = radio.size(size);
        }
        if let Some(text_size) = text_size.as_ref().and_then(DynProp::get) {
            radio = radio.text_size(text_size);
        }
        radio.into()
    })
}

/// One table column's layout facts, read once at build from the factory's `columns`
/// records (the header elements ride separately, in the headers list).
#[derive(Default)]
struct ColumnMeta {
    width: Option<iced::Length>,
    align_x: Option<Horizontal>,
    align_y: Option<Vertical>,
}

#[op2]
#[cppgc]
fn op_smudgy_widget_build_table(
    scope: &mut v8::PinScope,
    state: &mut OpState,
    props: v8::Local<v8::Object>,
    #[cppgc] headers: &ElementList,
    #[cppgc] cells: &ElementList,
) -> Element {
    let width = get_dyn_length_prop!(scope, state, props, "width");
    let padding = get_number_prop!(scope, props, "padding");
    let separator = get_number_prop!(scope, props, "separator");

    // Per-column layout from the `columns` records; the factory guarantees the headers
    // list is the same length. Anything unreadable simply leaves the column defaults.
    let metas: Vec<ColumnMeta> = {
        let key = ascii_str!("columns")
            .v8_string(scope)
            .expect("Could not allocate string")
            .into();
        let columns = props
            .get(scope, key)
            .and_then(|v| v8::Local::<v8::Array>::try_from(v).ok());
        let mut metas = Vec::new();
        if let Some(columns) = columns {
            for index in 0..columns.length() {
                let meta = columns
                    .get_index(scope, index)
                    .and_then(|v| v8::Local::<v8::Object>::try_from(v).ok())
                    .map_or_else(ColumnMeta::default, |record| ColumnMeta {
                        width: get_length_prop!(scope, record, "width"),
                        align_x: get_horizontal_prop!(scope, record, "align_x"),
                        align_y: get_vertical_prop!(scope, record, "align_y"),
                    });
                metas.push(meta);
            }
        }
        metas
    };

    let headers: Arc<Vec<Element>> = Arc::new(headers.0.take());
    let cells: Arc<Vec<Element>> = Arc::new(cells.0.take());
    // The factory keeps `columns` and the headers list the same length; clamping defends
    // the render thread against a direct op caller that doesn't (an out-of-range
    // `headers[index]` would panic mid-frame).
    let column_count = metas.len().min(headers.len());
    let row_count = cells.len().checked_div(column_count).unwrap_or(0);

    Element::new(move || {
        let columns = metas
            .iter()
            .take(column_count)
            .enumerate()
            .map(|(index, meta)| {
                let cells = cells.clone();
                let mut column =
                    iced::widget::table::column(headers[index].element(), move |row: usize| {
                        cells[row * column_count + index].element()
                    });
                if let Some(width) = meta.width {
                    column = column.width(width);
                }
                if let Some(align_x) = meta.align_x {
                    column = column.align_x(align_x);
                }
                if let Some(align_y) = meta.align_y {
                    column = column.align_y(align_y);
                }
                column
            });
        let mut table = iced::widget::table(columns, 0..row_count);
        if let Some(width) = width.as_ref().and_then(DynProp::get) {
            table = table.width(width);
        }
        #[allow(clippy::cast_possible_truncation)]
        if let Some(padding) = padding {
            table = table.padding(padding as f32);
        }
        #[allow(clippy::cast_possible_truncation)]
        if let Some(separator) = separator {
            table = table.separator(separator as f32);
        }
        table.into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(
            expand_command_autolinks("<go-north>"),
            "[go-north](<go-north>)"
        );
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
            "<say hi!>", // `!` -> not tokenized as inline HTML
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
        assert_eq!(
            bound(json!("hi")).display_text(),
            "hi",
            "strings render unquoted"
        );
        assert_eq!(bound(json!(42.5)).display_text(), "42.5");
        assert_eq!(bound(json!(true)).display_text(), "true");
        assert_eq!(
            bound(json!(null)).display_text(),
            "",
            "null/absent renders empty"
        );
        assert_eq!(bound(json!({ "a": 1 })).display_text(), r#"{"a":1}"#);

        let with_fallback = BoundProp {
            fallback: Some(Node::from(json!(0))),
            ..bound(json!(null))
        };
        assert_eq!(
            with_fallback.display_text(),
            "0",
            "fallback covers null/absent"
        );

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
        assert_eq!(
            f32_from_value(&node(json!("12"))),
            None,
            "no string-to-number coercion"
        );

        assert_eq!(
            length_from_value(&node(json!(120))),
            Some(iced::Length::Fixed(120.0))
        );
        assert_eq!(
            length_from_value(&node(json!("fill"))),
            Some(iced::Length::Fill)
        );
        assert_eq!(
            length_from_value(&node(json!("shrink"))),
            Some(iced::Length::Shrink)
        );
        assert_eq!(
            length_from_value(&node(json!("64"))),
            Some(iced::Length::Fixed(64.0))
        );
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
        assert_eq!(
            prop.get(),
            Some(75.0),
            "a live value wins over the fallback"
        );
        assert_eq!(DynProp::Static(1.0f32).get(), Some(1.0));
    }

    #[test]
    fn map_view_apply_and_doors_parse_from_store_nodes() {
        use serde_json::json;
        use smudgy_cloud::{ExitDirection, RoomNumber};

        let apply = style_applications_from_node(&Node::from(json!([
            {
                "style": "route",
                "rooms": [1, 2, 3],
                "exits": [{ "room": 3, "direction": "Up" }],
            },
            {
                "style": "visited",
                "rooms": [9],
                "area": [1, 2],
            }
        ])))
        .expect("apply entries parse");
        assert_eq!(apply[0].style, "route");
        assert_eq!(
            apply[0].rooms,
            vec![RoomNumber(1), RoomNumber(2), RoomNumber(3)]
        );
        assert_eq!(apply[0].exits[0].room, RoomNumber(3));
        assert_eq!(apply[0].exits[0].direction, ExitDirection::Up);
        assert_eq!(apply[0].area, None);
        assert_eq!(
            apply[1].area,
            Some(smudgy_cloud::AreaId(smudgy_cloud::Uuid::from_u64_pair(
                1, 2
            )))
        );

        let doors = door_states_from_node(&Node::from(json!([
            { "exit": { "room": 4, "direction": "East" }, "locked": true }
        ])))
        .expect("door states parse");
        assert_eq!(doors[0].exit.room, RoomNumber(4));
        assert_eq!(doors[0].exit.direction, ExitDirection::East);
        assert_eq!(doors[0].closed, None);
        assert_eq!(doors[0].locked, Some(true));

        let style = map_style_from_node(&Node::from(json!({
            "roomStroke": "#ff00ff",
            "connectionWidth": 2.0,
            "crossAreaLabelVisibility": "hover",
            "crossAreaLabelBackground": "rgba(7, 7, 6, 0.88)",
        })))
        .expect("style parses");
        assert_eq!(style.room_stroke.as_deref(), Some("#ff00ff"));
        assert_eq!(style.connection_width, Some(2.0));
        assert_eq!(style.room_fill, None);
        assert_eq!(
            style.cross_area_label_visibility,
            Some(smudgy_map_widget::CrossAreaLabelVisibility::Hover)
        );
        assert_eq!(
            style.cross_area_label_background.as_deref(),
            Some("rgba(7, 7, 6, 0.88)")
        );
    }

    /// The `area` scope in both accepted spellings: the `[hi, lo]` pair and
    /// the canonical UUID string resolve to the same internal id, and the
    /// string spelling survives a JSON text round trip — the store-binding
    /// wire, which the pair's BigInt halves cannot travel.
    #[test]
    fn map_view_apply_area_accepts_uuid_string_spelling() {
        use serde_json::json;

        let id: smudgy_cloud::Uuid = "67e55044-10b1-426f-9247-bb680e5fe0c8"
            .parse()
            .expect("literal uuid parses");
        let (hi, lo) = id.as_u64_pair();
        assert_eq!(id.to_string(), "67e55044-10b1-426f-9247-bb680e5fe0c8");

        let apply = style_applications_from_node(&Node::from(json!([
            { "style": "route", "rooms": [1], "area": id.to_string() },
            { "style": "route", "rooms": [2], "area": [1, 2] },
        ])))
        .expect("both spellings parse");
        assert_eq!(apply[0].area, Some(smudgy_cloud::AreaId(id)));
        assert_eq!(
            apply[0].area,
            Some(smudgy_cloud::AreaId(smudgy_cloud::Uuid::from_u64_pair(
                hi, lo
            ))),
            "the string resolves to the same id as its own u64 halves"
        );
        assert_eq!(
            apply[1].area,
            Some(smudgy_cloud::AreaId(smudgy_cloud::Uuid::from_u64_pair(
                1, 2
            )))
        );

        // Round trip through JSON text, simulating a store-bound apply array.
        let wire = serde_json::to_string(&json!([
            { "style": "route", "rooms": [3], "area": id.to_string() }
        ]))
        .expect("wire form serializes");
        let node = Node::from(
            serde_json::from_str::<serde_json::Value>(&wire).expect("wire form deserializes"),
        );
        let bound = style_applications_from_node(&node).expect("wire form parses");
        assert_eq!(bound[0].area, Some(smudgy_cloud::AreaId(id)));
    }

    /// A string-scoped entry behaves like the pair form end-to-end: its rooms
    /// are styled when the view resolves that area and ignored elsewhere.
    #[test]
    fn map_view_string_scoped_apply_resolves_and_scopes() {
        use serde_json::json;
        use smudgy_cloud::{AreaId, RoomNumber, Uuid};

        let id: Uuid = "67e55044-10b1-426f-9247-bb680e5fe0c8"
            .parse()
            .expect("literal uuid parses");
        let apply = style_applications_from_node(&Node::from(json!([
            { "style": "route", "rooms": [1], "area": id.to_string() },
        ])))
        .expect("string-scoped entry parses");

        let presentation = smudgy_map_widget::MapViewPresentation {
            styles: std::collections::HashMap::from([(
                "route".to_string(),
                smudgy_map_widget::MapStyle {
                    room_fill: Some("#111111".to_string()),
                    ..smudgy_map_widget::MapStyle::default()
                },
            )]),
            apply,
            ..smudgy_map_widget::MapViewPresentation::default()
        };
        let scoped = presentation.resolve(AreaId(id));
        assert!(scoped.rooms.contains_key(&RoomNumber(1)));
        let elsewhere = presentation.resolve(AreaId(Uuid::from_u64_pair(0, 9)));
        assert!(!elsewhere.rooms.contains_key(&RoomNumber(1)));
    }

    /// An `area` string that is not a UUID cannot resolve a scope, so that
    /// entry is dropped (with a warn-once report) while its siblings survive.
    /// A wrong-typed `area` remains a loud whole-parse error.
    #[test]
    fn map_view_malformed_area_string_skips_only_that_entry() {
        use serde_json::json;
        use smudgy_cloud::RoomNumber;

        let apply = style_applications_from_node(&Node::from(json!([
            { "style": "route", "rooms": [1], "area": "not-a-uuid" },
            { "style": "route", "rooms": [2] },
        ])))
        .expect("the list still parses");
        assert_eq!(apply.len(), 1, "the unresolvable entry is skipped");
        assert_eq!(apply[0].rooms, vec![RoomNumber(2)]);

        assert!(
            style_applications_from_node(&Node::from(json!([
                { "style": "route", "area": 5 }
            ])))
            .is_err(),
            "a non-string, non-pair area is a shape error, not a degradable value"
        );
    }

    /// Malformed structured values surface as `Err` (reported to the log by
    /// the callers), never as a silently-empty parse.
    #[test]
    fn map_view_negative_parses_error_instead_of_nulling() {
        use serde_json::json;

        // A wrong-typed field inside one entry fails that parse loudly.
        assert!(
            style_applications_from_node(&Node::from(json!([
                { "style": "route", "rooms": "not-an-array" }
            ])))
            .is_err()
        );
        // A missing required field (the style name) fails.
        assert!(style_applications_from_node(&Node::from(json!([{ "rooms": [1] }]))).is_err());
        // An unknown direction spelling fails the exit ref.
        assert!(
            door_states_from_node(&Node::from(json!([
                { "exit": { "room": 1, "direction": "Norf" } }
            ])))
            .is_err()
        );
        // The whole prop having the wrong shape fails.
        assert!(style_applications_from_node(&Node::from(json!({ "style": "route" }))).is_err());
        assert!(map_style_from_node(&Node::from(json!("gold"))).is_err());
        assert!(
            map_style_from_node(&Node::from(json!({
                "crossAreaLabelVisibility": "sometimes"
            })))
            .is_err()
        );
    }

    /// The bound-prop memo: an unchanged store snapshot must not re-parse
    /// (same `Arc`, cached value back), and a changed one must.
    #[test]
    fn serde_prop_caches_parses_on_snapshot_identity() {
        use serde_json::json;

        let prop = SerdeProp::Bound {
            prop: bound(json!([{ "style": "route", "rooms": [1] }])),
            name: "apply",
            parse: style_applications_from_node,
            cache: RefCell::new(None),
        };

        let first = prop.get().expect("initial snapshot parses");
        assert_eq!(first[0].rooms, vec![smudgy_cloud::RoomNumber(1)]);

        // Same snapshot: the memo must hit (observable as the same cached
        // Arc snapshot rather than a fresh parse).
        let _second = prop.get().expect("cached snapshot returns");
        if let SerdeProp::Bound {
            prop: inner, cache, ..
        } = &prop
        {
            let cached = cache.borrow();
            let (snapshot, _) = cached.as_ref().expect("cache is populated");
            assert!(
                Arc::ptr_eq(snapshot, &inner.cell.load()),
                "the memo must key on the live snapshot's identity"
            );

            drop(cached);
            inner.cell.set(json!([{ "style": "route", "rooms": [2] }]));
        }
        let third = prop.get().expect("new snapshot parses");
        assert_eq!(third[0].rooms, vec![smudgy_cloud::RoomNumber(2)]);

        // A malformed replacement value degrades to None (with a warn-once
        // report), not a stale cached value and not a panic.
        if let SerdeProp::Bound { prop: inner, .. } = &prop {
            inner.cell.set(json!([{ "rooms": "bad" }]));
        }
        assert_eq!(prop.get(), None);
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
            assert_eq!(
                extract_markdown_links(src),
                vec![],
                "expected no links in `{src}`"
            );
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

//! Host-owned automation editor controller.
//!
//! The controller deliberately depends on abstract editor and service channels, keeping
//! persistence and language-service state outside the widget. Writable JavaScript and
//! TypeScript documents use the upstream `iced-code-editor` adapter; read-only surfaces
//! remain on Smudgy's existing widgets and do not enter this lifecycle.

mod iced_surface;

#[allow(unused_imports)]
pub(super) use iced_surface::{IcedCodeEditorSurface, IcedEditorMessage};

use std::io::Read;
use std::time::{Duration, Instant};

use smudgy_script::language_service::{
    AcknowledgedState, AnalysisContextId, AutomationKind, CancelRequest, ChangeDocument, ClientId,
    CloseDocument, Command, CommandSequence, CompletionKind, CompletionResult, DefinitionResult,
    DefinitionTarget, Diagnostic, DiagnosticCode, DiagnosticsResult, DiskRevision, DocumentChanges,
    DocumentDescriptor, DocumentId, DocumentKey, DocumentKind, DocumentRef, DocumentRequest,
    DocumentResult, DocumentResultIdentity, DocumentStateIdentity, DocumentVersion, Event,
    EventEnvelope, FailureScope, FormattingOptions, FormattingRequest, GraphGeneration,
    HoverResult, Language, LanguageServiceLibrary, MAX_DOCUMENT_BYTES, MAX_PROJECT_SOURCE_FILES,
    MAX_PROJECT_SOURCE_TEXT_BYTES, MarkupKind, OpenDocument, OpenProject, PositionRequest,
    ProjectId, ProjectScope, ProjectSource, ProjectStateIdentity, ProjectStatus, RefreshProject,
    RequestId, SaveDocument, SignatureHelpResult, TextEdit, Utf16Position, Utf16Range, Validate,
    WorkerGeneration, validate_document_text,
};
use smudgy_script::language_service_worker::{
    LanguageServiceClient, LanguageServiceHost, LanguageServiceSendError,
};

/// Result of routing a worker event into the current editor state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EventDisposition {
    Applied,
    Stale,
    Invalid,
}

/// Language-intelligence availability never controls whether the text surface is usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ServiceStatus {
    Starting,
    Ready,
    Unavailable,
}

/// An editor-surface update and the exact ordered text changes it produced.
pub(super) struct SurfaceUpdate<Effect> {
    pub effect: Effect,
    pub changes: Option<DocumentChanges>,
    /// Latest completion position requested by the editor, expressed in the
    /// editor's scalar-column coordinates.
    pub completion: Option<CompletionIntent>,
    /// Latest call-signature position inferred from an edit or caret move,
    /// expressed in the editor's scalar-column coordinates.
    pub signature_help: Option<SignatureHelpIntent>,
    /// Pointer-derived hover intent. Passive editor messages leave it unchanged;
    /// leaving text or losing canvas focus clears the current hover lifecycle.
    pub hover: HoverUpdate,
    /// Whether text, caret, selection, or editor navigation changed the semantic
    /// context that transient intelligence was requested for.
    pub semantic_context_changed: bool,
    /// A user-requested definition position in the editor's scalar-column coordinates.
    pub definition: Option<ScalarPosition>,
    /// A user-requested whole-document format using the editor's current indentation style.
    pub formatting: Option<FormattingOptions>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ScalarPosition {
    pub line: u32,
    pub character: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(super) struct SurfacePoint {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(super) struct OverlayMetrics {
    pub viewport_width: f32,
    pub viewport_height: f32,
    pub viewport_scroll: f32,
    pub line_height: f32,
    pub char_width: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct CompletionIntent {
    pub position: ScalarPosition,
    pub anchor: SurfacePoint,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct SignatureHelpIntent {
    pub position: ScalarPosition,
    pub anchor: SurfacePoint,
    /// `(` and `,` explicitly begin a new signature-help lifecycle after
    /// Escape suppresses passive edit/caret retriggers.
    pub starts_new_lifecycle: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct HoverIntent {
    pub position: ScalarPosition,
    pub anchor: SurfacePoint,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(super) enum HoverUpdate {
    #[default]
    Unchanged,
    /// The pointer temporarily left source text. The accepted card gets a short
    /// grace period so the pointer can cross into the card itself.
    Leave,
    /// A hard invalidation, such as the editor canvas losing focus.
    Clear,
    At(HoverIntent),
}

const HOVER_DEBOUNCE: Duration = Duration::from_millis(300);
const HOVER_DISMISS_GRACE: Duration = Duration::from_millis(300);
const COMPLETION_MAX_VISIBLE_ROWS: usize = 12;
const COMPLETION_OVERSCAN_ROWS: usize = 4;
const COMPLETION_ROW_HEIGHT: f32 = 26.0;

#[derive(Debug, Clone, Copy)]
enum CompletionNavigation {
    Previous,
    Next,
    PageUp,
    PageDown,
    First,
    Last,
}

#[derive(Debug, Clone)]
struct CompletionNavigationUpdate {
    selected: usize,
    count: usize,
    scroll: Option<(iced::widget::Id, usize)>,
}

/// The minimum host seam every real automation editor surface must implement.
///
/// `changes` returned from [`Self::update`] are relative to the current document version.
/// A real surface must produce them from its authoritative buffer without keeping a second
/// mutable text copy in this controller.
pub(super) trait EditorSurface {
    type Message;
    type Effect;

    fn content(&self) -> String;
    fn update(&mut self, message: &Self::Message) -> SurfaceUpdate<Self::Effect>;
    fn apply_completion(
        &mut self,
        item: &smudgy_script::language_service::CompletionItem,
    ) -> SurfaceUpdate<Self::Effect>;
    /// Applies one simultaneous, already-fenced edit batch. `Err` means the
    /// surface could not represent the ranges; `Ok(None)` means a no-op.
    fn apply_text_edits(&mut self, edits: &[TextEdit]) -> Result<Option<DocumentChanges>, ()>;
    fn goto_position(&mut self, position: ScalarPosition) -> Self::Effect;
    fn reset(&mut self, text: &str, language: Language) -> Self::Effect;
    fn is_modified(&self) -> bool;
    fn mark_saved(&mut self);
    fn request_focus(&self);
    fn lose_focus(&mut self);
    fn is_dialog_open(&self) -> bool;
}

/// Narrow parent-side channel; framing, sequencing, and process ownership stay outside the
/// editor component.
pub(super) trait LanguageServiceChannel {
    type Error;

    fn send(&mut self, command: Command) -> Result<(), Self::Error>;
}

impl LanguageServiceChannel for LanguageServiceClient {
    type Error = LanguageServiceSendError;

    fn send(&mut self, command: Command) -> Result<(), Self::Error> {
        LanguageServiceClient::send(self, command).map(|_| ())
    }
}

/// Semantic kind of the single writable code document mounted in this window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum CodeDocument {
    Alias,
    Trigger,
    Hotkey,
    StandaloneModule,
    OwnedPackage,
}

/// Which saved-source graph may participate beneath the mounted editor overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LanguageProjectContext {
    Inline,
    Modules,
    OwnedPackage(String),
}

/// Stable window-lifetime identity for one saved source or scoped generated root.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum LanguageSourceKey {
    Module(String),
    OwnedPackage {
        package: String,
        subpath: String,
    },
    InlineAutomation {
        kind: CodeDocument,
        language: Language,
        folder_name: Option<String>,
        script_name: String,
    },
    InlineBridge,
}

/// One accepted cross-file definition jump. The origin state and mount fence the
/// queued navigation message across project refreshes, edits, and editor rebinds.
#[derive(Debug, Clone)]
pub struct DefinitionNavigation {
    origin: DocumentStateIdentity,
    origin_mount_generation: u64,
    target: DefinitionTarget,
}

/// The newest project refresh whose acknowledgement still owns the context transition.
///
/// The worker installs refreshes atomically, so the UI must not call a context current merely
/// because its command entered the channel. The command sequence disambiguates a failure (whose
/// failure scope describes the still-current graph), while the graph generation fences success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PendingLanguageProjectRefresh {
    context: LanguageProjectContext,
    graph_generation: GraphGeneration,
    command_sequence: CommandSequence,
    retries_remaining: u8,
    /// The inline bridge the refresh carries, recorded as installed by the same
    /// acknowledgement; `None` outside the inline context.
    inline_bridge: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectRefreshRetry {
    context: LanguageProjectContext,
    retries_remaining: u8,
}

struct PendingProjectSource {
    key: LanguageSourceKey,
    uri: String,
    language: Language,
    kind: DocumentKind,
    text: String,
}

/// The sources one project refresh installs, with the inline bridge among them.
struct LanguageProjectSources {
    sources: Vec<ProjectSource>,
    /// The bridge text the inline context's sources carry; `None` for the other contexts.
    inline_bridge: Option<String>,
}

#[derive(Debug, Clone, Copy)]
enum RequestKind {
    Diagnostics,
    Completion,
    Hover,
    SignatureHelp,
    Definition,
    Formatting,
}

#[derive(Debug, Default)]
struct OutstandingRequests {
    diagnostics: Option<RequestId>,
    completion: Option<RequestId>,
    hover: Option<RequestId>,
    signature_help: Option<RequestId>,
    definition: Option<RequestId>,
    formatting: Option<RequestId>,
}

impl OutstandingRequests {
    fn get(&self, kind: RequestKind) -> Option<RequestId> {
        match kind {
            RequestKind::Diagnostics => self.diagnostics,
            RequestKind::Completion => self.completion,
            RequestKind::Hover => self.hover,
            RequestKind::SignatureHelp => self.signature_help,
            RequestKind::Definition => self.definition,
            RequestKind::Formatting => self.formatting,
        }
    }

    fn set(&mut self, kind: RequestKind, request_id: Option<RequestId>) {
        *match kind {
            RequestKind::Diagnostics => &mut self.diagnostics,
            RequestKind::Completion => &mut self.completion,
            RequestKind::Hover => &mut self.hover,
            RequestKind::SignatureHelp => &mut self.signature_help,
            RequestKind::Definition => &mut self.definition,
            RequestKind::Formatting => &mut self.formatting,
        } = request_id;
    }

    fn clear(&mut self) {
        *self = Self::default();
    }

    fn take_matching(&mut self, request_id: RequestId) -> Option<RequestKind> {
        for kind in [
            RequestKind::Diagnostics,
            RequestKind::Completion,
            RequestKind::Hover,
            RequestKind::SignatureHelp,
            RequestKind::Definition,
            RequestKind::Formatting,
        ] {
            if self.get(kind) == Some(request_id) {
                self.set(kind, None);
                return Some(kind);
            }
        }
        None
    }
}

/// Accepted service data. Rendering and edit application are intentionally later adapter
/// concerns; retaining them here proves routing never uses a URI as document authority.
#[derive(Debug, Default)]
pub(super) struct ServiceResults {
    diagnostics: Vec<Diagnostic>,
    completion: Option<AcceptedCompletion>,
    hover: Option<AcceptedHover>,
    signature_help: Option<AcceptedSignatureHelp>,
    definition: Option<AcceptedDefinition>,
}

impl ServiceResults {
    fn clear(&mut self) {
        *self = Self::default();
    }
}

#[derive(Debug)]
struct AcceptedDefinition {
    origin: DocumentStateIdentity,
    result: DefinitionResult,
}

#[derive(Debug)]
struct AcceptedCompletion {
    identity: DocumentResultIdentity,
    result: CompletionResult,
    anchor: SurfacePoint,
    selected: usize,
    first_visible: usize,
    scroll_id: iced::widget::Id,
}

#[derive(Debug)]
struct AcceptedHover {
    identity: DocumentResultIdentity,
    source_position: Option<Utf16Position>,
    presentation: HoverPresentation,
    anchor: SurfacePoint,
}

#[derive(Debug)]
struct AcceptedSignatureHelp {
    identity: DocumentResultIdentity,
    request_position: Utf16Position,
    result: SignatureHelpResult,
    documentation: Option<HoverPresentation>,
    active_parameter_documentation: Option<HoverPresentation>,
    anchor: SurfacePoint,
    scroll_id: iced::widget::Id,
}

#[derive(Debug)]
enum HoverPresentation {
    PlainText(String),
    Markdown(Box<iced::widget::markdown::Content>),
}

fn rich_markdown_settings(
    viewer: &smudgy_widgets::SmudgyMarkdownViewer,
) -> iced::widget::markdown::Settings {
    let mut settings = viewer.settings_with_text_size(12.0);
    settings.style.inline_code_font = crate::assets::fonts::GEIST_MONO_VF;
    settings.style.code_block_font = crate::assets::fonts::GEIST_MONO_VF;
    settings.h1_size = 16.0.into();
    settings.h2_size = 15.0.into();
    settings.h3_size = 14.0.into();
    settings.h4_size = 13.0.into();
    settings.h5_size = 12.0.into();
    settings.h6_size = 12.0.into();
    settings.code_size = 11.0.into();
    settings.spacing = 7.0.into();
    settings
}

impl HoverPresentation {
    fn from_markup(markup: &smudgy_script::language_service::MarkupContent) -> Self {
        match markup.kind {
            MarkupKind::PlainText => Self::PlainText(markup.value.clone()),
            MarkupKind::Markdown => Self::Markdown(Box::new(
                iced::widget::markdown::Content::parse(&markup.value),
            )),
        }
    }

    fn estimated_lines(&self, chars_per_line: usize) -> usize {
        fn wrapped_text_lines(source: &str, chars_per_line: usize) -> usize {
            let chars_per_line = chars_per_line.max(1);
            source
                .lines()
                .map(|line| line.chars().count().max(1).div_ceil(chars_per_line))
                .sum::<usize>()
                .max(1)
        }

        fn markdown_text_lines(
            text: &iced::widget::markdown::Text,
            chars_per_line: usize,
            style: iced::widget::markdown::Style,
        ) -> usize {
            let visible = text
                .spans(style)
                .iter()
                .map(|span| span.text.as_ref())
                .collect::<String>();
            wrapped_text_lines(&visible, chars_per_line)
        }

        fn markdown_item_lines(
            item: &iced::widget::markdown::Item,
            chars_per_line: usize,
            style: iced::widget::markdown::Style,
        ) -> usize {
            use iced::widget::markdown::{Bullet, Item};

            match item {
                Item::Heading(_, text) | Item::Paragraph(text) => {
                    markdown_text_lines(text, chars_per_line, style)
                }
                // Fenced code is horizontally scrollable, so each source line
                // contributes one vertical line regardless of signature length.
                Item::CodeBlock { lines, .. } => lines.len().max(1) + 1,
                Item::List { bullets, .. } => bullets
                    .iter()
                    .map(|bullet| match bullet {
                        Bullet::Point { items } | Bullet::Task { items, .. } => items
                            .iter()
                            .map(|item| markdown_item_lines(item, chars_per_line, style))
                            .sum::<usize>()
                            .max(1),
                    })
                    .sum::<usize>()
                    .max(1),
                Item::Image { alt, .. } => markdown_text_lines(alt, chars_per_line, style),
                Item::Quote(items) => items
                    .iter()
                    .map(|item| markdown_item_lines(item, chars_per_line, style))
                    .sum::<usize>()
                    .max(1),
                Item::Rule => 1,
                Item::Table { rows, .. } => rows.len().max(1) + 1,
            }
        }

        match self {
            Self::PlainText(value) => wrapped_text_lines(value, chars_per_line),
            Self::Markdown(content) => {
                let viewer = smudgy_widgets::SmudgyMarkdownViewer::current();
                let style = rich_markdown_settings(&viewer).style;
                content
                    .items()
                    .iter()
                    .map(|item| markdown_item_lines(item, chars_per_line, style))
                    .sum::<usize>()
                    .max(1)
            }
        }
    }
}

fn rich_markup_view<'a>(
    presentation: &'a HoverPresentation,
) -> crate::theme::Element<'a, iced::widget::markdown::Uri> {
    match presentation {
        HoverPresentation::PlainText(value) => iced::widget::text(value.as_str())
            .size(12.0)
            .wrapping(iced::widget::text::Wrapping::WordOrGlyph)
            .into(),
        HoverPresentation::Markdown(content) => {
            let viewer = smudgy_widgets::SmudgyMarkdownViewer::current();
            let settings = rich_markdown_settings(&viewer);
            viewer.view(content.items(), settings)
        }
    }
}

impl AcceptedHover {
    fn new(
        identity: DocumentResultIdentity,
        source_position: Option<Utf16Position>,
        result: HoverResult,
        anchor: SurfacePoint,
    ) -> Self {
        let presentation = HoverPresentation::from_markup(&result.contents);
        Self {
            identity,
            source_position,
            presentation,
            anchor,
        }
    }
}

impl AcceptedSignatureHelp {
    fn new(
        identity: DocumentResultIdentity,
        request_position: Utf16Position,
        result: SignatureHelpResult,
        anchor: SurfacePoint,
    ) -> Self {
        let documentation = result
            .documentation
            .as_ref()
            .map(HoverPresentation::from_markup);
        let active_parameter_documentation = result
            .active_parameter
            .and_then(|index| result.parameters.get(usize::from(index)))
            .and_then(|parameter| parameter.documentation.as_ref())
            .map(HoverPresentation::from_markup);
        Self {
            identity,
            request_position,
            result,
            documentation,
            active_parameter_documentation,
            anchor,
            scroll_id: iced::widget::Id::unique(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct PendingCompletion {
    position: Utf16Position,
    anchor: SurfacePoint,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct PendingSignatureHelp {
    position: Utf16Position,
    anchor: SurfacePoint,
}

#[derive(Debug, Clone, Copy)]
struct PendingHover {
    position: Utf16Position,
    anchor: SurfacePoint,
    ready_at: Instant,
}

#[derive(Debug, Clone, Copy)]
struct PendingHoverDismiss {
    identity: DocumentResultIdentity,
    ready_at: Instant,
}

/// Exact UI authority for one completion row. A delayed click must not apply
/// the same index from a later result or a remounted document.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CompletionSelection {
    document_id: DocumentId,
    mount_generation: u64,
    identity: DocumentResultIdentity,
    index: usize,
}

/// Exact UI authority for the visible row reported by one completion scrollable.
/// Late scroll notifications from a replaced result or remounted document are inert.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CompletionViewportTarget {
    document_id: DocumentId,
    mount_generation: u64,
    identity: DocumentResultIdentity,
    first_visible: usize,
}

/// Exact UI authority for pointer interaction with one rendered hover card.
/// Delayed enter/exit messages from a replaced result or remounted document are inert.
#[derive(Debug, Clone, Copy)]
pub(crate) struct HoverOverlayTarget {
    document_id: DocumentId,
    mount_generation: u64,
    identity: DocumentResultIdentity,
}

/// Exact UI authority for an inert link inside one rendered signature card.
/// Keeping this distinct from hover authority prevents future link behavior
/// from accidentally authorizing an action against the wrong overlay kind.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SignatureOverlayTarget {
    document_id: DocumentId,
    mount_generation: u64,
    identity: DocumentResultIdentity,
}

/// Owns one authoritative editor surface and generation-fenced language-service view.
pub(super) struct AutomationCodeEditor<S, C>
where
    S: EditorSurface,
    C: LanguageServiceChannel,
{
    surface: S,
    document: DocumentDescriptor,
    service: Option<C>,
    service_state: Option<DocumentStateIdentity>,
    project_state: Option<ProjectStateIdentity>,
    worker_generation: Option<WorkerGeneration>,
    /// Last document state successfully queued to the worker. This may be newer
    /// than the last acknowledgement, but it is the exact state a later close
    /// must name after the worker drains its FIFO.
    service_document: Option<DocumentRef>,
    /// A full close/open synchronization is needed after transient backpressure.
    resync_pending: bool,
    status: ServiceStatus,
    outstanding: OutstandingRequests,
    results: ServiceResults,
    pending_completion: Option<PendingCompletion>,
    completion_request_anchor: Option<SurfacePoint>,
    pending_signature_help: Option<PendingSignatureHelp>,
    signature_help_request_position: Option<Utf16Position>,
    signature_help_request_anchor: Option<SurfacePoint>,
    signature_help_suppressed: bool,
    hover_position: Option<Utf16Position>,
    pending_hover: Option<PendingHover>,
    hover_request_anchor: Option<SurfacePoint>,
    hover_overlay_interactive: Option<DocumentResultIdentity>,
    pending_hover_dismiss: Option<PendingHoverDismiss>,
    pending_definition: Option<Utf16Position>,
    pending_formatting: Option<FormattingOptions>,
    service_edit_applied: bool,
    closed: bool,
}

impl<S, C> AutomationCodeEditor<S, C>
where
    S: EditorSurface,
    C: LanguageServiceChannel,
{
    pub fn new(surface: S, document: DocumentDescriptor, service: Option<C>) -> Self {
        let mut editor = Self {
            surface,
            document,
            service,
            service_state: None,
            project_state: None,
            worker_generation: None,
            service_document: None,
            resync_pending: false,
            status: ServiceStatus::Starting,
            outstanding: OutstandingRequests::default(),
            results: ServiceResults::default(),
            pending_completion: None,
            completion_request_anchor: None,
            pending_signature_help: None,
            signature_help_request_position: None,
            signature_help_request_anchor: None,
            signature_help_suppressed: false,
            hover_position: None,
            pending_hover: None,
            hover_request_anchor: None,
            hover_overlay_interactive: None,
            pending_hover_dismiss: None,
            pending_definition: None,
            pending_formatting: None,
            service_edit_applied: false,
            closed: false,
        };
        editor.open_service_document();
        editor
    }

    pub fn document(&self) -> &DocumentDescriptor {
        &self.document
    }

    pub fn content(&self) -> String {
        self.surface.content()
    }

    pub fn is_modified(&self) -> bool {
        self.surface.is_modified()
    }

    pub fn service_status(&self) -> ServiceStatus {
        self.status
    }

    pub fn has_language_service(&self) -> bool {
        self.service.is_some()
    }

    pub fn results(&self) -> &ServiceResults {
        &self.results
    }

    fn visible_diagnostics(&self) -> impl Iterator<Item = &Diagnostic> {
        self.results
            .diagnostics
            .iter()
            .filter(|diagnostic| !self.active_signature_hides(diagnostic))
    }

    fn active_signature_hides(&self, diagnostic: &Diagnostic) -> bool {
        let Some(help) = &self.results.signature_help else {
            return false;
        };
        self.service_state == Some(help.identity.state)
            && diagnostic.source.as_deref() == Some("typescript")
            && matches!(&diagnostic.code, Some(DiagnosticCode::Number(1005)))
            && diagnostic.message == "')' expected."
            && diagnostic.range.start == diagnostic.range.end
            && diagnostic.range.start == help.request_position
            && help.result.applicable_range.end == help.request_position
    }

    fn signature_help_is_current(&self, identity: DocumentResultIdentity) -> bool {
        self.service_state == Some(identity.state)
            && self
                .results
                .signature_help
                .as_ref()
                .is_some_and(|help| help.identity == identity)
    }

    pub fn update(&mut self, message: &S::Message) -> S::Effect {
        self.update_with_change(message).0
    }

    /// Updates the surface and reports whether its authoritative text changed.
    pub fn update_with_change(&mut self, message: &S::Message) -> (S::Effect, bool) {
        let SurfaceUpdate {
            effect,
            changes,
            completion,
            signature_help,
            hover,
            semantic_context_changed,
            definition,
            formatting,
        } = self.surface.update(message);
        if self.closed {
            return (effect, false);
        }
        // Tick, pointer hover, focus hand-off to an overlay button, and scrolling
        // must not destroy a valid completion. Only mutations to text/caret/
        // selection/navigation invalidate the semantic request context.
        if semantic_context_changed {
            self.clear_transient_intelligence();
        }
        let changed = changes.is_some();
        if let Some(changes) = changes {
            self.document_changed(changes);
        }
        if let Some(intent) = completion
            && let Some(position) = scalar_to_utf16(intent.position, &self.surface.content())
        {
            self.clear_completion();
            self.clear_hover();
            self.pending_completion = Some(PendingCompletion {
                position,
                anchor: intent.anchor,
            });
        }
        if let Some(intent) = signature_help {
            if intent.starts_new_lifecycle {
                self.signature_help_suppressed = false;
            }
            if !self.signature_help_suppressed {
                self.clear_signature_help();
                self.clear_hover();
                if let Some(position) = scalar_to_utf16(intent.position, &self.surface.content()) {
                    self.pending_signature_help = Some(PendingSignatureHelp {
                        position,
                        anchor: intent.anchor,
                    });
                }
            }
        }
        match hover {
            HoverUpdate::Unchanged => {}
            HoverUpdate::Leave => self.leave_hover(Instant::now()),
            HoverUpdate::Clear => {
                self.clear_signature_help();
                self.clear_hover();
            }
            HoverUpdate::At(intent) => self.observe_hover_intent(intent, Instant::now()),
        }
        if let Some(position) = definition {
            self.pending_definition = scalar_to_utf16(position, &self.surface.content());
            self.results.definition = None;
            self.outstanding.set(RequestKind::Definition, None);
        }
        if let Some(options) = formatting {
            self.pending_formatting = Some(options);
            self.outstanding.set(RequestKind::Formatting, None);
        }
        (effect, changed)
    }

    /// Applies one current completion item through the authoritative surface.
    fn apply_completion(
        &mut self,
        index: usize,
        expected: DocumentResultIdentity,
    ) -> Option<(S::Effect, bool)> {
        let accepted = self.results.completion.as_ref()?;
        if accepted.identity != expected || self.service_state != Some(expected.state) {
            return None;
        }
        let item = accepted.result.items.get(index)?.clone();
        let SurfaceUpdate {
            effect,
            changes,
            completion: _,
            signature_help,
            hover: _,
            semantic_context_changed: _,
            definition: _,
            formatting: _,
        } = self.surface.apply_completion(&item);
        let changed = changes.is_some();
        if let Some(changes) = changes {
            self.document_changed(changes);
        }
        if let Some(intent) = signature_help {
            if intent.starts_new_lifecycle {
                self.signature_help_suppressed = false;
            }
            if !self.signature_help_suppressed
                && let Some(position) = scalar_to_utf16(intent.position, &self.surface.content())
            {
                self.pending_signature_help = Some(PendingSignatureHelp {
                    position,
                    anchor: intent.anchor,
                });
            }
        }
        self.clear_completion();
        self.clear_hover();
        Some((effect, changed))
    }

    fn apply_selected_completion(&mut self) -> Option<(S::Effect, bool)> {
        let completion = self.results.completion.as_ref()?;
        self.apply_completion(completion.selected, completion.identity)
    }

    fn navigate_completion(
        &mut self,
        navigation: CompletionNavigation,
        visible_rows: usize,
    ) -> Option<CompletionNavigationUpdate> {
        let Some(completion) = &mut self.results.completion else {
            return None;
        };
        let count = completion.result.items.len();
        if count == 0 {
            return None;
        }
        let page_rows = visible_rows.saturating_sub(1).max(1);
        let previous_first_visible = completion.first_visible;
        completion.selected = match navigation {
            CompletionNavigation::Previous => completion
                .selected
                .checked_sub(1)
                .unwrap_or(count.saturating_sub(1)),
            CompletionNavigation::Next => completion.selected.saturating_add(1) % count,
            CompletionNavigation::PageUp => completion.selected.saturating_sub(page_rows),
            CompletionNavigation::PageDown => completion
                .selected
                .saturating_add(page_rows)
                .min(count.saturating_sub(1)),
            CompletionNavigation::First => 0,
            CompletionNavigation::Last => count.saturating_sub(1),
        };
        completion.first_visible = reveal_completion_selection(
            completion.first_visible,
            completion.selected,
            count,
            visible_rows,
        );
        Some(CompletionNavigationUpdate {
            selected: completion.selected,
            count,
            scroll: (completion.first_visible != previous_first_visible)
                .then(|| (completion.scroll_id.clone(), completion.first_visible)),
        })
    }

    fn completion_viewport_changed(
        &mut self,
        expected: DocumentResultIdentity,
        first_visible: usize,
    ) -> bool {
        let Some(completion) = &mut self.results.completion else {
            return false;
        };
        if completion.identity != expected || self.service_state != Some(expected.state) {
            return false;
        }
        completion.first_visible =
            first_visible.min(completion.result.items.len().saturating_sub(1));
        true
    }

    pub fn rebind(&mut self, document: DocumentDescriptor, text: &str) -> S::Effect {
        let project_changed = self.document.document.key.project != document.document.key.project;
        self.close_service_document();
        let effect = self.surface.reset(text, document.language);
        self.surface.mark_saved();
        self.document = document;
        self.closed = false;
        if project_changed {
            self.project_state = None;
            self.worker_generation = None;
        }
        self.clear_service_state();
        self.pending_completion = None;
        self.completion_request_anchor = None;
        self.pending_signature_help = None;
        self.signature_help_request_position = None;
        self.signature_help_request_anchor = None;
        self.signature_help_suppressed = false;
        self.hover_position = None;
        self.pending_hover = None;
        self.hover_request_anchor = None;
        self.hover_overlay_interactive = None;
        self.pending_hover_dismiss = None;
        self.pending_definition = None;
        self.pending_formatting = None;
        self.service_edit_applied = false;
        self.status = ServiceStatus::Starting;
        if self.service_document.is_some() {
            self.resync_pending = true;
            self.retry_service_sync();
        } else {
            self.open_service_document();
        }
        effect
    }

    /// Call only after persistence has durably accepted this exact editor version.
    pub fn mark_saved(&mut self, disk_revision: DiskRevision) {
        if self.closed {
            return;
        }
        self.surface.mark_saved();
        self.document.disk_revision = Some(disk_revision);
        let command = Command::SaveDocument(SaveDocument {
            document: self.document.document,
            text: self.surface.content(),
            disk_revision,
        });
        if !self.send(command) {
            // The service rejected this save snapshot. A coalesced close/open
            // retry carries the same content and disk revision.
            self.schedule_service_resync();
        }
    }

    pub fn request_focus(&self) {
        self.surface.request_focus();
    }

    pub fn lose_focus(&mut self) {
        self.surface.lose_focus();
        self.clear_completion();
        self.clear_signature_help();
        self.clear_hover();
        self.pending_definition = None;
        self.pending_formatting = None;
        self.outstanding.clear();
        self.results.clear();
    }

    pub fn is_dialog_open(&self) -> bool {
        self.surface.is_dialog_open()
    }

    pub fn request_diagnostics(&mut self, request_id: RequestId) -> bool {
        self.begin_request(RequestKind::Diagnostics, request_id, |identity| {
            Command::RequestDiagnostics(DocumentRequest { identity })
        })
    }

    pub fn request_completion(&mut self, request_id: RequestId, position: Utf16Position) -> bool {
        let sent = self.begin_request(RequestKind::Completion, request_id, |identity| {
            Command::RequestCompletion(PositionRequest { identity, position })
        });
        if sent {
            self.completion_request_anchor = None;
        }
        sent
    }

    /// Sends the newest editor-triggered completion after its exact document
    /// version has been acknowledged by the worker.
    pub fn request_pending_completion(&mut self, request_id: RequestId) -> bool {
        let Some(pending) = self.pending_completion.take() else {
            return false;
        };
        if self.begin_request(RequestKind::Completion, request_id, |identity| {
            Command::RequestCompletion(PositionRequest {
                identity,
                position: pending.position,
            })
        }) {
            self.completion_request_anchor = Some(pending.anchor);
            true
        } else {
            self.pending_completion = Some(pending);
            false
        }
    }

    /// Sends the newest caret-derived signature probe after its exact document
    /// version has been acknowledged by the worker.
    pub fn request_pending_signature_help(&mut self, request_id: RequestId) -> bool {
        let Some(pending) = self.pending_signature_help.take() else {
            return false;
        };
        if self.begin_request(RequestKind::SignatureHelp, request_id, |identity| {
            Command::RequestSignatureHelp(PositionRequest {
                identity,
                position: pending.position,
            })
        }) {
            self.signature_help_request_position = Some(pending.position);
            self.signature_help_request_anchor = Some(pending.anchor);
            true
        } else {
            self.pending_signature_help = Some(pending);
            false
        }
    }

    pub fn request_pending_definition(&mut self, request_id: RequestId) -> bool {
        let Some(position) = self.pending_definition.take() else {
            return false;
        };
        if self.request_definition(request_id, position) {
            true
        } else {
            self.pending_definition = Some(position);
            false
        }
    }

    pub fn request_pending_formatting(&mut self, request_id: RequestId) -> bool {
        let Some(options) = self.pending_formatting.take() else {
            return false;
        };
        if self.request_formatting(request_id, options) {
            true
        } else {
            self.pending_formatting = Some(options);
            false
        }
    }

    pub fn request_hover(&mut self, request_id: RequestId, position: Utf16Position) -> bool {
        let sent = self.begin_request(RequestKind::Hover, request_id, |identity| {
            Command::RequestHover(PositionRequest { identity, position })
        });
        if sent {
            self.hover_request_anchor = None;
        }
        sent
    }

    fn request_pending_hover(&mut self, request_id: RequestId, now: Instant) -> bool {
        let Some(pending) = self.pending_hover.take() else {
            return false;
        };
        if now < pending.ready_at {
            self.pending_hover = Some(pending);
            return false;
        }
        if self.begin_request(RequestKind::Hover, request_id, |identity| {
            Command::RequestHover(PositionRequest {
                identity,
                position: pending.position,
            })
        }) {
            self.hover_request_anchor = Some(pending.anchor);
            true
        } else {
            self.pending_hover = Some(pending);
            false
        }
    }

    fn clear_completion(&mut self) {
        self.pending_completion = None;
        self.completion_request_anchor = None;
        self.outstanding.set(RequestKind::Completion, None);
        self.results.completion = None;
    }

    fn clear_signature_help(&mut self) {
        self.pending_signature_help = None;
        self.signature_help_request_position = None;
        self.signature_help_request_anchor = None;
        self.outstanding.set(RequestKind::SignatureHelp, None);
        self.results.signature_help = None;
    }

    fn clear_hover(&mut self) {
        self.hover_position = None;
        self.pending_hover = None;
        self.hover_request_anchor = None;
        self.hover_overlay_interactive = None;
        self.pending_hover_dismiss = None;
        self.outstanding.set(RequestKind::Hover, None);
        self.results.hover = None;
    }

    fn clear_hover_request(&mut self) {
        self.pending_hover = None;
        self.hover_request_anchor = None;
        self.outstanding.set(RequestKind::Hover, None);
    }

    fn restore_hover_position_to_accepted(&mut self) {
        self.hover_position = self
            .results
            .hover
            .as_ref()
            .and_then(|hover| hover.source_position);
    }

    fn leave_hover(&mut self, now: Instant) {
        self.clear_hover_request();
        self.restore_hover_position_to_accepted();
        let Some(hover) = &self.results.hover else {
            self.hover_overlay_interactive = None;
            self.pending_hover_dismiss = None;
            return;
        };
        if self.hover_overlay_interactive == Some(hover.identity) {
            return;
        }
        self.pending_hover_dismiss = Some(PendingHoverDismiss {
            identity: hover.identity,
            ready_at: now + HOVER_DISMISS_GRACE,
        });
    }

    fn hover_overlay_entered(&mut self, identity: DocumentResultIdentity) -> bool {
        let Some(hover) = &self.results.hover else {
            return false;
        };
        if hover.identity != identity {
            return false;
        }
        let source_position = hover.source_position;
        self.clear_hover_request();
        self.hover_position = source_position;
        self.hover_overlay_interactive = Some(identity);
        self.pending_hover_dismiss = None;
        true
    }

    fn hover_overlay_exited(&mut self, identity: DocumentResultIdentity, now: Instant) -> bool {
        if self
            .results
            .hover
            .as_ref()
            .is_none_or(|hover| hover.identity != identity)
        {
            return false;
        }
        self.hover_overlay_interactive = None;
        self.pending_hover_dismiss = Some(PendingHoverDismiss {
            identity,
            ready_at: now + HOVER_DISMISS_GRACE,
        });
        true
    }

    fn expire_hover_dismiss(&mut self, now: Instant) -> bool {
        let Some(pending) = self.pending_hover_dismiss else {
            return false;
        };
        if self
            .results
            .hover
            .as_ref()
            .is_none_or(|hover| hover.identity != pending.identity)
        {
            self.pending_hover_dismiss = None;
            return false;
        }
        if now < pending.ready_at || self.hover_overlay_interactive == Some(pending.identity) {
            return false;
        }
        self.clear_hover();
        true
    }

    fn clear_transient_intelligence(&mut self) {
        self.clear_completion();
        self.clear_signature_help();
        self.clear_hover();
    }

    fn observe_hover_intent(&mut self, intent: HoverIntent, now: Instant) {
        // Completion is the active interaction; background hover motion must
        // never replace it or open a competing overlay. Signature help is
        // passive and remains accepted underneath a deliberate hover so it can
        // return as soon as the hover card closes.
        if self.pending_completion.is_some()
            || self.outstanding.completion.is_some()
            || self.results.completion.is_some()
        {
            self.clear_hover();
            return;
        }
        let Some(position) = scalar_to_utf16(intent.position, &self.surface.content()) else {
            self.leave_hover(now);
            return;
        };
        if self.hover_overlay_interactive.is_some() {
            return;
        }
        self.pending_hover_dismiss = None;
        if self.hover_position == Some(position) {
            return;
        }
        // Keep the accepted card visible while a replacement hover debounces.
        // This avoids a distracting blank flash while moving between symbols.
        self.clear_hover_request();
        self.hover_position = Some(position);
        self.pending_hover = Some(PendingHover {
            position,
            anchor: intent.anchor,
            ready_at: now + HOVER_DEBOUNCE,
        });
    }

    pub(super) fn dismiss_pointer_overlays(&mut self) {
        self.clear_completion();
        self.clear_hover();
    }

    pub(super) fn dismiss_overlays(&mut self) {
        self.clear_completion();
        self.clear_signature_help();
        self.clear_hover();
        self.signature_help_suppressed = true;
    }

    pub fn request_definition(&mut self, request_id: RequestId, position: Utf16Position) -> bool {
        self.begin_request(RequestKind::Definition, request_id, |identity| {
            Command::RequestDefinition(PositionRequest { identity, position })
        })
    }

    pub fn request_formatting(
        &mut self,
        request_id: RequestId,
        options: FormattingOptions,
    ) -> bool {
        self.begin_request(RequestKind::Formatting, request_id, |identity| {
            Command::RequestFormatting(FormattingRequest { identity, options })
        })
    }

    fn take_definition(&mut self) -> Option<AcceptedDefinition> {
        self.results.definition.take()
    }

    fn goto_utf16_position(&mut self, position: Utf16Position) -> Option<S::Effect> {
        let position = utf16_to_scalar(position, &self.surface.content())?;
        Some(self.surface.goto_position(position))
    }

    fn is_current_state(&self, state: DocumentStateIdentity) -> bool {
        !self.closed && self.service_state == Some(state)
    }

    fn take_service_edit_applied(&mut self) -> bool {
        std::mem::take(&mut self.service_edit_applied)
    }

    pub fn cancel_request(&mut self, request_id: RequestId) -> bool {
        let Some(kind) = self.outstanding.take_matching(request_id) else {
            return false;
        };
        match kind {
            RequestKind::Completion => self.completion_request_anchor = None,
            RequestKind::SignatureHelp => {
                self.signature_help_request_position = None;
                self.signature_help_request_anchor = None;
            }
            RequestKind::Hover => {
                self.hover_request_anchor = None;
                self.restore_hover_position_to_accepted();
            }
            RequestKind::Diagnostics | RequestKind::Definition | RequestKind::Formatting => {}
        }
        let project = self.document.document.key.project;
        self.send(Command::Cancel(CancelRequest {
            project,
            request_id,
        }))
    }

    pub fn apply_service_event(&mut self, envelope: &EventEnvelope) -> EventDisposition {
        if envelope.validate().is_err() {
            return EventDisposition::Invalid;
        }
        if self.closed {
            return EventDisposition::Stale;
        }

        match &envelope.event {
            Event::StateAcknowledged(state) => self.apply_acknowledgement(*state),
            Event::WorkerRestarted { worker_generation } => {
                if self
                    .worker_generation
                    .is_some_and(|current| *worker_generation <= current)
                {
                    return EventDisposition::Stale;
                }
                self.worker_generation = Some(*worker_generation);
                self.project_state = None;
                self.service_document = None;
                self.resync_pending = false;
                self.clear_service_state();
                self.status = ServiceStatus::Starting;
                self.open_service_document();
                EventDisposition::Applied
            }
            Event::Diagnostics(result) => {
                if !self.accepts_result(result, RequestKind::Diagnostics) {
                    return EventDisposition::Stale;
                }
                let text = self.surface.content();
                if !diagnostic_ranges_fit(
                    &result.result,
                    self.document.document.key.document_id,
                    &text,
                ) {
                    return EventDisposition::Invalid;
                }
                self.results.diagnostics.clone_from(&result.result.items);
                self.outstanding.set(RequestKind::Diagnostics, None);
                EventDisposition::Applied
            }
            Event::Completion(result) => {
                if !self.accepts_result(result, RequestKind::Completion) {
                    return EventDisposition::Stale;
                }
                if !completion_ranges_fit(&result.result, &self.surface.content()) {
                    self.clear_completion();
                    return EventDisposition::Invalid;
                }
                let anchor = self.completion_request_anchor.take().unwrap_or_default();
                self.clear_hover();
                self.results.completion =
                    (!result.result.items.is_empty()).then(|| AcceptedCompletion {
                        identity: result.identity,
                        result: result.result.clone(),
                        anchor,
                        selected: 0,
                        first_visible: 0,
                        scroll_id: iced::widget::Id::unique(),
                    });
                self.outstanding.set(RequestKind::Completion, None);
                EventDisposition::Applied
            }
            Event::Hover(result) => {
                if !self.accepts_result(result, RequestKind::Hover) {
                    return EventDisposition::Stale;
                }
                let text = self.surface.content();
                if !result
                    .result
                    .as_ref()
                    .is_none_or(|hover| hover.range.is_none_or(|range| range_fits(range, &text)))
                {
                    self.clear_hover();
                    return EventDisposition::Invalid;
                }
                let anchor = self.hover_request_anchor.take().unwrap_or_default();
                let source_position = self.hover_position;
                let identity = result.identity;
                self.hover_overlay_interactive = None;
                self.pending_hover_dismiss = None;
                self.results.hover = result
                    .result
                    .clone()
                    .map(|result| AcceptedHover::new(identity, source_position, result, anchor));
                if self.results.hover.is_none() {
                    self.hover_position = None;
                }
                self.outstanding.set(RequestKind::Hover, None);
                EventDisposition::Applied
            }
            Event::SignatureHelp(result) => {
                if !self.accepts_result(result, RequestKind::SignatureHelp) {
                    return EventDisposition::Stale;
                }
                let text = self.surface.content();
                if !result.result.as_ref().is_none_or(|help| {
                    range_fits(help.applicable_range, &text)
                        && self
                            .signature_help_request_position
                            .is_some_and(|position| {
                                range_fits(
                                    Utf16Range {
                                        start: position,
                                        end: position,
                                    },
                                    &text,
                                ) && help.applicable_range.start <= position
                                    && position <= help.applicable_range.end
                            })
                }) {
                    self.clear_signature_help();
                    return EventDisposition::Invalid;
                }
                let request_position = self.signature_help_request_position.take();
                let anchor = self
                    .signature_help_request_anchor
                    .take()
                    .unwrap_or_default();
                let identity = result.identity;
                self.results.signature_help = match (request_position, result.result.clone()) {
                    (Some(position), Some(help)) => {
                        Some(AcceptedSignatureHelp::new(identity, position, help, anchor))
                    }
                    _ => None,
                };
                self.outstanding.set(RequestKind::SignatureHelp, None);
                EventDisposition::Applied
            }
            Event::Definition(result) => {
                if !self.accepts_result(result, RequestKind::Definition) {
                    return EventDisposition::Stale;
                }
                let text = self.surface.content();
                if !definition_ranges_fit(
                    &result.result,
                    self.document.document.key.document_id,
                    &text,
                ) {
                    return EventDisposition::Invalid;
                }
                self.results.definition = Some(AcceptedDefinition {
                    origin: result.identity.state,
                    result: result.result.clone(),
                });
                self.outstanding.set(RequestKind::Definition, None);
                EventDisposition::Applied
            }
            Event::Formatting(result) => {
                if !self.accepts_result(result, RequestKind::Formatting) {
                    return EventDisposition::Stale;
                }
                let text = self.surface.content();
                if !simultaneous_text_edits_fit(&result.result.edits, &text) {
                    return EventDisposition::Invalid;
                }
                self.outstanding.set(RequestKind::Formatting, None);
                let Ok(changes) = self.surface.apply_text_edits(&result.result.edits) else {
                    return EventDisposition::Invalid;
                };
                if let Some(changes) = changes {
                    self.document_changed(changes);
                    self.service_edit_applied = true;
                }
                EventDisposition::Applied
            }
            Event::ProjectStatus(event) => {
                if event.identity.project != self.document.document.key.project
                    || self
                        .worker_generation
                        .is_some_and(|generation| generation != event.identity.worker_generation)
                {
                    return EventDisposition::Stale;
                }
                if self
                    .service_state
                    .is_some_and(|state| !project_identity_matches_document(event.identity, state))
                {
                    return EventDisposition::Stale;
                }
                self.worker_generation = Some(event.identity.worker_generation);
                self.project_state = Some(event.identity);
                self.status = match &event.status {
                    ProjectStatus::Ready => ServiceStatus::Ready,
                    ProjectStatus::Degraded { .. } => {
                        self.outstanding.clear();
                        self.clear_transient_intelligence();
                        self.results.clear();
                        ServiceStatus::Unavailable
                    }
                };
                EventDisposition::Applied
            }
            Event::RequestFailed(failure) => self.apply_request_failure(failure.scope),
        }
    }

    pub fn close(&mut self) {
        self.close_service_document();
    }

    fn document_changed(&mut self, changes: DocumentChanges) {
        let previous = self.document.document;
        let Some(new_version) = DocumentVersion::new(previous.version.get().saturating_add(1))
        else {
            self.request_service_close();
            self.clear_service_state();
            self.status = ServiceStatus::Unavailable;
            return;
        };

        self.document.document.version = new_version;
        self.clear_service_state();
        self.status = ServiceStatus::Starting;
        if changes.validate().is_err()
            || self.resync_pending
            || self.service_document != Some(previous)
        {
            self.schedule_service_resync();
            return;
        }
        let command = Command::ChangeDocument(ChangeDocument {
            document: previous,
            new_version,
            changes,
        });
        if self.send(command) {
            self.service_document = Some(self.document.document);
        } else {
            self.resync_pending = true;
        }
    }

    fn open_service_document(&mut self) {
        if self.service.is_none() {
            self.service_document = None;
            self.resync_pending = false;
            self.status = ServiceStatus::Unavailable;
            return;
        }
        let text = self.surface.content();
        if validate_document_text(&text).is_err() {
            self.service_document = None;
            self.resync_pending = false;
            self.status = ServiceStatus::Unavailable;
            return;
        }
        let command = Command::OpenDocument(OpenDocument {
            descriptor: self.document.clone(),
            text,
        });
        if self.send(command) {
            self.service_document = Some(self.document.document);
            self.resync_pending = false;
        } else {
            self.service_document = None;
            self.resync_pending = true;
        }
    }

    fn close_service_document(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        if let Some(document) = self.service_document
            && self.send(Command::CloseDocument(CloseDocument { document }))
        {
            self.service_document = None;
        }
        self.resync_pending = false;
        self.clear_service_state();
    }

    fn request_service_close(&mut self) {
        let Some(document) = self.service_document else {
            return;
        };
        if self.send(Command::CloseDocument(CloseDocument { document })) {
            self.service_document = None;
        }
    }

    fn schedule_service_resync(&mut self) {
        self.resync_pending = true;
        self.retry_service_sync();
    }

    /// Retries a coalesced full-snapshot synchronization after a channel send
    /// failed. A close which was queued successfully is never repeated; if the
    /// following open was rejected, the next tick resumes there.
    pub fn retry_service_sync(&mut self) {
        if self.closed || !self.resync_pending {
            return;
        }
        if let Some(document) = self.service_document {
            if !self.send(Command::CloseDocument(CloseDocument { document })) {
                return;
            }
            self.service_document = None;
        }
        self.status = ServiceStatus::Starting;
        self.open_service_document();
    }

    fn clear_service_state(&mut self) {
        self.service_state = None;
        self.completion_request_anchor = None;
        self.pending_signature_help = None;
        self.signature_help_request_position = None;
        self.signature_help_request_anchor = None;
        self.clear_hover();
        self.pending_definition = None;
        self.pending_formatting = None;
        self.outstanding.clear();
        self.results.clear();
    }

    fn send(&mut self, command: Command) -> bool {
        let Some(service) = &mut self.service else {
            self.status = ServiceStatus::Unavailable;
            return false;
        };
        if service.send(command).is_err() {
            self.status = ServiceStatus::Unavailable;
            return false;
        }
        true
    }

    fn begin_request(
        &mut self,
        kind: RequestKind,
        request_id: RequestId,
        command: impl FnOnce(DocumentResultIdentity) -> Command,
    ) -> bool {
        let Some(state) = self.service_state else {
            return false;
        };
        if self.closed || self.status != ServiceStatus::Ready {
            return false;
        }
        let identity = DocumentResultIdentity { state, request_id };
        if !self.send(command(identity)) {
            return false;
        }
        self.outstanding.set(kind, Some(request_id));
        true
    }

    fn accepts_result<T>(&self, result: &DocumentResult<T>, kind: RequestKind) -> bool {
        let Some(state) = self.service_state else {
            return false;
        };
        let Some(request_id) = self.outstanding.get(kind) else {
            return false;
        };
        result.identity.is_current_for(&state, request_id)
            && result
                .analyzed_uri
                .as_deref()
                .is_none_or(|uri| uri == self.document.uri)
    }

    fn apply_acknowledgement(&mut self, state: AcknowledgedState) -> EventDisposition {
        if let AcknowledgedState::ProjectRefreshed(project) = state {
            if project.project != self.document.document.key.project
                || self
                    .worker_generation
                    .is_some_and(|expected| project.worker_generation != expected)
                || self.project_state.is_some_and(|current| {
                    project.graph_generation <= current.graph_generation
                        || project.service_generation < current.service_generation
                })
            {
                return EventDisposition::Stale;
            }
            self.worker_generation = Some(project.worker_generation);
            self.project_state = Some(project);
            if let Some(mut current) = self.service_state {
                current.graph_generation = project.graph_generation;
                current.service_generation = project.service_generation;
                current.worker_generation = project.worker_generation;
                self.service_state = Some(current);
            }
            self.outstanding.clear();
            self.completion_request_anchor = None;
            self.clear_signature_help();
            self.clear_hover();
            self.results.clear();
            self.status = ServiceStatus::Ready;
            return EventDisposition::Applied;
        }
        let document_state = match state {
            AcknowledgedState::DocumentOpened(state)
            | AcknowledgedState::DocumentChanged(state)
            | AcknowledgedState::DocumentSaved(state) => state,
            _ => return EventDisposition::Stale,
        };
        if document_state.document != self.document.document {
            return EventDisposition::Stale;
        }
        if self
            .worker_generation
            .is_some_and(|expected| document_state.worker_generation != expected)
        {
            return EventDisposition::Stale;
        }
        if let Some(project) = self.project_state
            && (project.project != document_state.document.key.project
                || project.worker_generation != document_state.worker_generation
                || document_state.graph_generation < project.graph_generation
                || document_state.service_generation < project.service_generation)
        {
            return EventDisposition::Stale;
        }
        if let Some(current) = self.service_state
            && (document_state.graph_generation < current.graph_generation
                || document_state.service_generation < current.service_generation)
        {
            return EventDisposition::Stale;
        }
        self.worker_generation = Some(document_state.worker_generation);
        self.project_state = Some(ProjectStateIdentity {
            project: document_state.document.key.project,
            graph_generation: document_state.graph_generation,
            service_generation: document_state.service_generation,
            worker_generation: document_state.worker_generation,
        });
        self.service_document = Some(document_state.document);
        self.resync_pending = false;
        self.service_state = Some(document_state);
        if self.status == ServiceStatus::Starting {
            self.status = ServiceStatus::Ready;
        }
        EventDisposition::Applied
    }

    fn apply_request_failure(&mut self, scope: FailureScope) -> EventDisposition {
        match scope {
            FailureScope::Document(identity) => {
                let Some(state) = self.service_state else {
                    return EventDisposition::Stale;
                };
                if identity.state != state {
                    return EventDisposition::Stale;
                }
                let Some(kind) = self.outstanding.take_matching(identity.request_id) else {
                    return EventDisposition::Stale;
                };
                match kind {
                    RequestKind::Completion => self.completion_request_anchor = None,
                    RequestKind::SignatureHelp => {
                        self.signature_help_request_position = None;
                        self.signature_help_request_anchor = None;
                        self.results.signature_help = None;
                    }
                    RequestKind::Hover => {
                        self.hover_request_anchor = None;
                        self.restore_hover_position_to_accepted();
                    }
                    RequestKind::Diagnostics
                    | RequestKind::Definition
                    | RequestKind::Formatting => {}
                }
                EventDisposition::Applied
            }
            FailureScope::Project(project) => {
                let Some(state) = self.service_state else {
                    return EventDisposition::Stale;
                };
                if project.project != state.document.key.project
                    || project.graph_generation != state.graph_generation
                    || project.service_generation != state.service_generation
                    || project.worker_generation != state.worker_generation
                {
                    return EventDisposition::Stale;
                }
                self.outstanding.clear();
                self.clear_transient_intelligence();
                self.results.clear();
                self.status = ServiceStatus::Unavailable;
                EventDisposition::Applied
            }
            FailureScope::Worker { worker_generation } => {
                if self
                    .worker_generation
                    .is_some_and(|current| worker_generation != current)
                {
                    return EventDisposition::Stale;
                }
                self.worker_generation = Some(worker_generation);
                self.project_state = None;
                self.service_document = None;
                self.resync_pending = false;
                self.clear_service_state();
                self.status = ServiceStatus::Unavailable;
                EventDisposition::Applied
            }
        }
    }
}

/// Concrete writable editor type used by the Automations window.
pub(super) type ActiveCodeEditor =
    AutomationCodeEditor<IcedCodeEditorSurface, LanguageServiceClient>;

#[cfg(test)]
impl<C: LanguageServiceChannel> AutomationCodeEditor<IcedCodeEditorSurface, C> {
    /// Number of times the host released this editor's keyboard focus.
    pub(super) fn focus_losses(&self) -> usize {
        self.surface.focus_losses
    }
}

/// An upstream editor message fenced to the document that produced it.
///
/// Some upstream actions complete asynchronously (notably clipboard reads). The
/// identity prevents their eventual result from mutating a document opened later.
#[derive(Debug, Clone)]
pub(crate) struct BoundEditorMessage {
    pub(super) document_id: DocumentId,
    pub(super) mount_generation: u64,
    pub(super) message: IcedEditorMessage,
}

impl ActiveCodeEditor {
    /// Renders the upstream widget using Smudgy's nested-theme bridge.
    pub(super) fn view(&self) -> crate::theme::Element<'_, IcedEditorMessage> {
        self.surface.view()
    }

    fn sync_theme_from_prefs(&mut self) {
        self.surface.sync_theme_from_prefs();
    }

    fn overlay_metrics(&self) -> OverlayMetrics {
        self.surface.overlay_metrics()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct OverlayPlacement {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

fn reveal_completion_selection(
    first_visible: usize,
    selected: usize,
    count: usize,
    visible_rows: usize,
) -> usize {
    let visible_rows = visible_rows.max(1).min(count.max(1));
    let max_first = count.saturating_sub(visible_rows);
    let first_visible = first_visible.min(max_first);
    if selected < first_visible {
        selected
    } else if selected >= first_visible.saturating_add(visible_rows) {
        selected.saturating_add(1).saturating_sub(visible_rows)
    } else {
        first_visible
    }
    .min(max_first)
}

fn completion_scroll_task(
    scroll_id: iced::widget::Id,
    first_visible: usize,
) -> iced::Task<super::Message> {
    iced::widget::operation::scroll_to(
        scroll_id,
        iced::widget::scrollable::AbsoluteOffset {
            x: 0.0,
            y: first_visible as f32 * COMPLETION_ROW_HEIGHT,
        },
    )
}

const fn completion_kind_label(kind: CompletionKind) -> &'static str {
    match kind {
        CompletionKind::Text => "text",
        CompletionKind::Method => "method",
        CompletionKind::Function => "function",
        CompletionKind::Constructor => "constructor",
        CompletionKind::Field => "field",
        CompletionKind::Variable => "variable",
        CompletionKind::Class => "class",
        CompletionKind::Interface => "interface",
        CompletionKind::TypeAlias => "type alias",
        CompletionKind::Module => "module",
        CompletionKind::Property => "property",
        CompletionKind::Unit => "unit",
        CompletionKind::Value => "value",
        CompletionKind::Enum => "enum",
        CompletionKind::Keyword => "keyword",
        CompletionKind::Snippet => "snippet",
        CompletionKind::Color => "color",
        CompletionKind::File => "file",
        CompletionKind::Reference => "reference",
        CompletionKind::Folder => "folder",
        CompletionKind::EnumMember => "enum member",
        CompletionKind::Constant => "constant",
        CompletionKind::Struct => "struct",
        CompletionKind::Event => "event",
        CompletionKind::Operator => "operator",
        CompletionKind::TypeParameter => "type parameter",
    }
}

fn color_luminance(color: iced::Color) -> f32 {
    fn linear(channel: f32) -> f32 {
        if channel <= 0.04045 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    }

    0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
}

fn contrast_ratio(first: iced::Color, second: iced::Color) -> f32 {
    let first = color_luminance(first);
    let second = color_luminance(second);
    (first.max(second) + 0.05) / (first.min(second) + 0.05)
}

fn mix_color(first: iced::Color, second: iced::Color, amount: f32) -> iced::Color {
    let amount = amount.clamp(0.0, 1.0);
    iced::Color {
        r: (second.r - first.r).mul_add(amount, first.r),
        g: (second.g - first.g).mul_add(amount, first.g),
        b: (second.b - first.b).mul_add(amount, first.b),
        a: (second.a - first.a).mul_add(amount, first.a),
    }
}

fn completion_surface_color(theme: &crate::theme::Theme) -> iced::Color {
    let overlay = theme.styles.general.overlay_background;
    let background = theme.styles.general.background;
    iced::Color {
        r: overlay
            .r
            .mul_add(overlay.a, background.r * (1.0 - overlay.a)),
        g: overlay
            .g
            .mul_add(overlay.a, background.g * (1.0 - overlay.a)),
        b: overlay
            .b
            .mul_add(overlay.a, background.b * (1.0 - overlay.a)),
        a: 1.0,
    }
}

fn completion_row_background(
    theme: &crate::theme::Theme,
    selected: bool,
    status: iced::widget::button::Status,
) -> iced::Color {
    let surface = completion_surface_color(theme);
    let theme_text = iced::Color {
        a: 1.0,
        ..theme.styles.text.normal
    };
    let black = iced::Color::BLACK;
    let white = iced::Color::WHITE;
    let fallback = if contrast_ratio(black, surface) >= contrast_ratio(white, surface) {
        black
    } else {
        white
    };
    let wash = if contrast_ratio(theme_text, surface) >= 4.5 {
        theme_text
    } else {
        fallback
    };
    let amount = match (selected, status) {
        (false, iced::widget::button::Status::Active) => 0.04,
        (false, iced::widget::button::Status::Hovered) => 0.10,
        (false, iced::widget::button::Status::Pressed) => 0.14,
        (false, iced::widget::button::Status::Disabled) => 0.04,
        (true, iced::widget::button::Status::Active) => 0.18,
        (true, iced::widget::button::Status::Hovered) => 0.24,
        (true, iced::widget::button::Status::Pressed) => 0.28,
        (true, iced::widget::button::Status::Disabled) => 0.18,
    };
    mix_color(surface, wash, amount)
}

fn completion_row_style(
    selected: bool,
) -> impl Fn(&crate::theme::Theme, iced::widget::button::Status) -> iced::widget::button::Style {
    move |theme, status| iced::widget::button::Style {
        background: Some(completion_row_background(theme, selected, status).into()),
        text_color: theme.styles.text.normal,
        ..iced::widget::button::Style::default()
    }
}

fn readable_semantic_color(
    candidate: iced::Color,
    theme: &crate::theme::Theme,
    minimum_contrast: f32,
) -> iced::Color {
    let background = completion_surface_color(theme);
    let candidate = iced::Color {
        a: 1.0,
        ..candidate
    };
    if contrast_ratio(candidate, background) >= minimum_contrast {
        return candidate;
    }

    let text = iced::Color {
        a: 1.0,
        ..theme.styles.text.normal
    };
    for step in 1..=12 {
        let adjusted = mix_color(candidate, text, step as f32 / 12.0);
        if contrast_ratio(adjusted, background) >= minimum_contrast {
            return adjusted;
        }
    }

    let black = iced::Color::BLACK;
    let white = iced::Color::WHITE;
    if contrast_ratio(black, background) >= contrast_ratio(white, background) {
        black
    } else {
        white
    }
}

fn completion_kind_color(theme: &crate::theme::Theme, kind: CompletionKind) -> iced::Color {
    let light_surface = color_luminance(completion_surface_color(theme)) > 0.5;
    let candidate = match (light_surface, kind) {
        (true, CompletionKind::Method | CompletionKind::Function | CompletionKind::Constructor) => {
            iced::Color::from_rgb8(0x79, 0x5E, 0x26)
        }
        (
            true,
            CompletionKind::Class
            | CompletionKind::Interface
            | CompletionKind::TypeAlias
            | CompletionKind::Enum
            | CompletionKind::Struct
            | CompletionKind::TypeParameter,
        ) => iced::Color::from_rgb8(0x26, 0x7F, 0x99),
        (
            true,
            CompletionKind::Field
            | CompletionKind::Variable
            | CompletionKind::Property
            | CompletionKind::Value
            | CompletionKind::EnumMember
            | CompletionKind::Constant,
        ) => iced::Color::from_rgb8(0x00, 0x10, 0x80),
        (
            true,
            CompletionKind::Module
            | CompletionKind::File
            | CompletionKind::Reference
            | CompletionKind::Folder,
        ) => iced::Color::from_rgb8(0xA3, 0x15, 0x15),
        (true, CompletionKind::Keyword | CompletionKind::Operator) => {
            iced::Color::from_rgb8(0x00, 0x00, 0xFF)
        }
        (true, CompletionKind::Unit | CompletionKind::Color) => {
            iced::Color::from_rgb8(0x09, 0x86, 0x58)
        }
        (true, CompletionKind::Event) => iced::Color::from_rgb8(0xAF, 0x00, 0xDB),
        (true, CompletionKind::Text | CompletionKind::Snippet) => theme.styles.text.normal,
        (false, kind) => match kind {
            CompletionKind::Method | CompletionKind::Function | CompletionKind::Constructor => {
                iced::Color::from_rgb8(0xDC, 0xDC, 0xAA)
            }
            CompletionKind::Class
            | CompletionKind::Interface
            | CompletionKind::TypeAlias
            | CompletionKind::Enum
            | CompletionKind::Struct
            | CompletionKind::TypeParameter => iced::Color::from_rgb8(0x4E, 0xC9, 0xB0),
            CompletionKind::Field
            | CompletionKind::Variable
            | CompletionKind::Property
            | CompletionKind::Value
            | CompletionKind::EnumMember
            | CompletionKind::Constant => iced::Color::from_rgb8(0x9C, 0xDC, 0xFE),
            CompletionKind::Module
            | CompletionKind::File
            | CompletionKind::Reference
            | CompletionKind::Folder => iced::Color::from_rgb8(0xCE, 0x91, 0x78),
            CompletionKind::Keyword | CompletionKind::Operator => {
                iced::Color::from_rgb8(0xC5, 0x86, 0xC0)
            }
            CompletionKind::Unit | CompletionKind::Color => {
                iced::Color::from_rgb8(0xB5, 0xCE, 0xA8)
            }
            CompletionKind::Event => iced::Color::from_rgb8(0xD1, 0x69, 0x69),
            CompletionKind::Text | CompletionKind::Snippet => theme.styles.text.normal,
        },
    };
    readable_semantic_color(candidate, theme, 3.0)
}

fn completion_kind_style(
    kind: CompletionKind,
    deprecated: bool,
) -> impl Fn(&crate::theme::Theme) -> iced::widget::text::Style {
    move |theme| {
        let color = completion_kind_color(theme, kind);
        let color = if deprecated {
            let faded = mix_color(color, completion_surface_color(theme), 0.24);
            readable_semantic_color(faded, theme, 2.25)
        } else {
            color
        };
        iced::widget::text::Style { color: Some(color) }
    }
}

fn concise_completion_text(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let head = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

fn completion_desired_height(count: usize) -> f32 {
    8.0 + count.min(COMPLETION_MAX_VISIBLE_ROWS) as f32 * COMPLETION_ROW_HEIGHT
}

fn fit_overlay_vertically(
    anchor_y: f32,
    line_height: f32,
    viewport_height: f32,
    desired_height: f32,
    prefer_below: bool,
) -> (f32, f32) {
    const GAP: f32 = 4.0;
    let anchor_y = anchor_y.clamp(0.0, viewport_height);
    let anchor_bottom = (anchor_y + line_height).min(viewport_height);
    let above = (anchor_y - GAP).max(0.0);
    let below = (viewport_height - anchor_bottom - GAP).max(0.0);
    let show_below = if prefer_below {
        below >= desired_height || (above < desired_height && below >= above)
    } else {
        !(above >= desired_height || (below < desired_height && above >= below))
    };
    let available = if show_below { below } else { above };
    let height = desired_height.min(available).max(1.0);
    let y = if show_below {
        anchor_bottom + GAP
    } else {
        (anchor_y - GAP - height).max(0.0)
    };
    (height, y)
}

fn completion_placement(
    anchor: SurfacePoint,
    metrics: OverlayMetrics,
    desired_height: f32,
) -> OverlayPlacement {
    let viewport_width = metrics.viewport_width.max(8.0);
    let viewport_height = metrics.viewport_height.max(8.0);
    let width = 520.0_f32.min((viewport_width - 8.0).max(1.0));
    let adjusted_y = (anchor.y - metrics.viewport_scroll).clamp(0.0, viewport_height);
    let (height, y) = fit_overlay_vertically(
        adjusted_y,
        metrics.line_height,
        viewport_height,
        desired_height,
        true,
    );
    let max_x = (viewport_width - width - 4.0).max(4.0);
    let x = anchor.x.clamp(4.0, max_x);
    OverlayPlacement {
        x,
        y,
        width,
        height,
    }
}

fn hover_placement(
    anchor: SurfacePoint,
    metrics: OverlayMetrics,
    desired_height: f32,
) -> OverlayPlacement {
    let viewport_width = metrics.viewport_width.max(8.0);
    let viewport_height = metrics.viewport_height.max(8.0);
    let width = 520.0_f32.min((viewport_width - 8.0).max(1.0));
    let adjusted_y = (anchor.y - metrics.viewport_scroll).clamp(0.0, viewport_height);
    let (height, y) = fit_overlay_vertically(
        adjusted_y,
        metrics.line_height,
        viewport_height,
        desired_height,
        false,
    );
    let gap_x = (metrics.char_width * 0.5).max(2.0);
    let max_x = (viewport_width - width - 4.0).max(0.0);
    let right = anchor.x + gap_x;
    let left = anchor.x - width - gap_x;
    let x = if right <= max_x {
        right
    } else if left >= 0.0 {
        left
    } else {
        right.clamp(0.0, max_x)
    };
    OverlayPlacement {
        x,
        y,
        width,
        height,
    }
}

fn anchor_is_visible(anchor: SurfacePoint, metrics: OverlayMetrics) -> bool {
    let adjusted_y = anchor.y - metrics.viewport_scroll;
    adjusted_y >= -metrics.line_height && adjusted_y <= metrics.viewport_height
}

fn signature_overlay_should_render(
    signature: &AcceptedSignatureHelp,
    hover: Option<&AcceptedHover>,
    metrics: OverlayMetrics,
) -> bool {
    anchor_is_visible(signature.anchor, metrics)
        && !hover.is_some_and(|hover| anchor_is_visible(hover.anchor, metrics))
}

fn signature_desired_height(signature: &AcceptedSignatureHelp, metrics: OverlayMetrics) -> f32 {
    let available_width = 520.0_f32.min((metrics.viewport_width - 8.0).max(1.0));
    let chars_per_line =
        ((available_width - 24.0).max(1.0) / metrics.char_width.max(4.0)).floor() as usize;
    let signature_chars = signature.result.prefix.chars().count()
        + signature.result.suffix.chars().count()
        + signature
            .result
            .parameters
            .iter()
            .map(|parameter| parameter.label.chars().count())
            .sum::<usize>()
        + signature
            .result
            .separator
            .chars()
            .count()
            .saturating_mul(signature.result.parameters.len().saturating_sub(1));
    let signature_lines = signature_chars.max(1).div_ceil(chars_per_line.max(1));
    let documentation_lines = signature
        .active_parameter_documentation
        .iter()
        .chain(signature.documentation.iter())
        .map(|documentation| documentation.estimated_lines(chars_per_line))
        .sum::<usize>();
    (18.0 + signature_lines as f32 * 18.0 + documentation_lines as f32 * 18.0).clamp(42.0, 240.0)
}

fn allocate_stacked_overlay_heights(
    available: f32,
    signature_desired: f32,
    completion_desired: f32,
) -> (f32, f32) {
    const BETWEEN: f32 = 4.0;
    const SIGNATURE_MIN: f32 = 42.0;
    const COMPLETION_MIN: f32 = 34.0;

    let usable = (available - BETWEEN).max(0.0);
    if usable == 0.0 {
        return (0.0, 0.0);
    }
    let signature_min = signature_desired.min(SIGNATURE_MIN);
    let completion_min = completion_desired.min(COMPLETION_MIN);
    let minimum_total = signature_min + completion_min;
    if usable < minimum_total {
        let signature = usable * signature_min / minimum_total.max(1.0);
        return (signature, usable - signature);
    }

    let extra = usable - minimum_total;
    let signature_gap = (signature_desired - signature_min).max(0.0);
    let completion_gap = (completion_desired - completion_min).max(0.0);
    let total_gap = signature_gap + completion_gap;
    if total_gap == 0.0 {
        return (signature_min, completion_min);
    }
    let distributed = extra.min(total_gap);
    let signature_extra = distributed * signature_gap / total_gap;
    let completion_extra = distributed - signature_extra;
    (
        signature_min + signature_extra,
        completion_min + completion_extra,
    )
}

/// Places simultaneous signature and completion cards as one visual unit.
/// Prefer the conventional split around the source line. Near an edge, stack
/// both on the roomier side so neither card vanishes. No card may cover the
/// source line or the other card.
fn coordinated_signature_completion_placements(
    signature_anchor: SurfacePoint,
    signature_desired: f32,
    completion_anchor: SurfacePoint,
    completion_desired: f32,
    metrics: OverlayMetrics,
) -> (OverlayPlacement, OverlayPlacement) {
    const GAP: f32 = 4.0;
    const MIN_USABLE_CARD: f32 = 20.0;

    let viewport_height = metrics.viewport_height.max(8.0);
    let line_height = metrics.line_height.max(0.0);
    let signature_anchor_y =
        (signature_anchor.y - metrics.viewport_scroll).clamp(0.0, viewport_height);
    let completion_anchor_y =
        (completion_anchor.y - metrics.viewport_scroll).clamp(0.0, viewport_height);
    let source_top = signature_anchor_y.min(completion_anchor_y);
    let source_bottom = (signature_anchor_y + line_height)
        .max(completion_anchor_y + line_height)
        .min(viewport_height);
    let above_end = (source_top - GAP).max(0.0);
    let below_start = (source_bottom + GAP).min(viewport_height);
    let above = above_end;
    let below = viewport_height - below_start;

    let mut signature = hover_placement(signature_anchor, metrics, signature_desired);
    let mut completion = completion_placement(completion_anchor, metrics, completion_desired);
    let split_signature_height = signature_desired.min(above);
    let split_completion_height = completion_desired.min(below);
    if split_signature_height >= MIN_USABLE_CARD && split_completion_height >= MIN_USABLE_CARD {
        signature.height = split_signature_height;
        signature.y = above_end - split_signature_height;
        completion.height = split_completion_height;
        completion.y = below_start;
        return (signature, completion);
    }

    let stack_above =
        allocate_stacked_overlay_heights(above, signature_desired, completion_desired);
    let stack_below =
        allocate_stacked_overlay_heights(below, signature_desired, completion_desired);
    let above_is_usable = stack_above.0 >= MIN_USABLE_CARD && stack_above.1 >= MIN_USABLE_CARD;
    let below_is_usable = stack_below.0 >= MIN_USABLE_CARD && stack_below.1 >= MIN_USABLE_CARD;
    if above_is_usable || below_is_usable {
        if above_is_usable && (!below_is_usable || above >= below) {
            signature.height = stack_above.0;
            completion.height = stack_above.1;
            completion.y = above_end - completion.height;
            signature.y = completion.y - GAP - signature.height;
        } else {
            signature.height = stack_below.0;
            completion.height = stack_below.1;
            completion.y = below_start;
            signature.y = completion.y + completion.height + GAP;
        }
        return (signature, completion);
    }

    // There is not enough room for two usable cards on either side. Keep each
    // in its conventional region; the view will clip their scrollable bodies.
    signature.height = split_signature_height;
    signature.y = above_end - split_signature_height;
    completion.height = split_completion_height;
    completion.y = below_start;
    (signature, completion)
}

fn completion_placement_with_signature(
    completion: &AcceptedCompletion,
    signature: Option<&AcceptedSignatureHelp>,
    metrics: OverlayMetrics,
) -> OverlayPlacement {
    let completion_desired = completion_desired_height(completion.result.items.len());
    signature
        .filter(|signature| anchor_is_visible(signature.anchor, metrics))
        .map(|signature| {
            coordinated_signature_completion_placements(
                signature.anchor,
                signature_desired_height(signature, metrics),
                completion.anchor,
                completion_desired,
                metrics,
            )
            .1
        })
        .unwrap_or_else(|| completion_placement(completion.anchor, metrics, completion_desired))
}

fn completion_visible_rows(
    completion: &AcceptedCompletion,
    signature: Option<&AcceptedSignatureHelp>,
    metrics: OverlayMetrics,
) -> usize {
    let count = completion.result.items.len();
    if count == 0 {
        return 1;
    }
    let placement = completion_placement_with_signature(completion, signature, metrics);
    (((placement.height - 8.0).max(1.0) / COMPLETION_ROW_HEIGHT).floor() as usize)
        .clamp(1, count.min(COMPLETION_MAX_VISIBLE_ROWS))
}

impl super::AutomationsWindow {
    /// Applies Smudgy's current palette to the active upstream editor without
    /// disturbing its text, caret, selection, history, or language-service state.
    pub(crate) fn sync_code_editor_theme(&mut self) {
        if let Some(editor) = &mut self.code_editor {
            editor.sync_theme_from_prefs();
        }
    }

    pub(super) fn bind_code_editor_message(
        &self,
        message: IcedEditorMessage,
    ) -> Option<BoundEditorMessage> {
        Some(BoundEditorMessage {
            document_id: self
                .code_editor
                .as_ref()?
                .document()
                .document
                .key
                .document_id,
            mount_generation: self.code_editor_mount_generation,
            message,
        })
    }

    pub(super) fn code_editor_message_is_current(&self, message: &BoundEditorMessage) -> bool {
        self.code_editor.as_ref().is_some_and(|editor| {
            editor.document().document.key.document_id == message.document_id
                && self.code_editor_mount_generation == message.mount_generation
        })
    }

    /// Opens or rebinds the window's single writable code editor.
    pub(super) fn bind_code_editor(
        &mut self,
        text: &str,
        language: Language,
        kind: CodeDocument,
    ) -> iced::Task<super::Message> {
        self.code_editor_mount_generation = self
            .code_editor_mount_generation
            .checked_add(1)
            .unwrap_or(1);
        self.pointer_over_code_editor = false;
        let needs_service = supports_language_service(language);
        let context = language_project_context(self, kind);
        let context_changed = self.language_project_target_context.as_ref() != Some(&context);
        self.language_project_target_context = Some(context.clone());
        let refresh_needed = needs_service
            && (!self.language_project_is_installed_or_pending(&context)
                || (context == LanguageProjectContext::Inline && self.inline_bridge_is_stale()));
        let bound_with_service = self
            .code_editor
            .as_ref()
            .is_some_and(AutomationCodeEditor::has_language_service);
        if self.code_editor.is_some()
            && (bound_with_service != needs_service || (needs_service && context_changed))
        {
            self.clear_code_editor();
        }
        if refresh_needed && let Some(client) = self.ensure_language_service() {
            self.install_language_project(context.clone(), &client);
        }
        let descriptor = self.code_document_descriptor(language, kind);
        let task = if let Some(editor) = &mut self.code_editor {
            editor.rebind(descriptor, text)
        } else {
            let surface = IcedCodeEditorSurface::new(text, language);
            let client = if needs_service {
                self.ensure_language_service()
            } else {
                None
            };
            self.code_editor = Some(AutomationCodeEditor::new(surface, descriptor, client));
            iced::Task::none()
        };
        self.bind_code_editor_task(task)
    }

    /// Returns the current code text. Writable code panes always bind before rendering.
    pub(super) fn code_editor_text(&self) -> String {
        self.code_editor
            .as_ref()
            .map_or_else(String::new, AutomationCodeEditor::content)
    }

    /// Renders the active writable editor plus its visible language-service
    /// status, current problems, and clickable completion candidates.
    pub(super) fn code_editor_view(&self, height: iced::Length) -> super::Elem<'_> {
        let Some(editor) = &self.code_editor else {
            return iced::widget::container(iced::widget::text(""))
                .height(height)
                .into();
        };
        let mount_generation = self.code_editor_mount_generation;

        let status = if !supports_language_service(editor.document().language) {
            crate::i18n::t!("automation-code-intelligence-not-applicable")
        } else {
            match editor.service_status() {
                ServiceStatus::Starting => {
                    crate::i18n::t!("automation-code-intelligence-starting")
                }
                ServiceStatus::Ready => crate::i18n::t!("automation-code-intelligence-ready"),
                ServiceStatus::Unavailable => {
                    crate::i18n::t!("automation-code-intelligence-unavailable")
                }
            }
        };

        let document_id = editor.document().document.key.document_id;
        let mut editor_layers: Vec<super::Elem<'_>> = vec![
            iced::widget::container(editor.view().map(move |message| {
                super::Message::CodeEditorAction(BoundEditorMessage {
                    document_id,
                    mount_generation,
                    message,
                })
            }))
            .width(iced::Length::Fill)
            .height(height)
            .into(),
        ];
        // Keep a permanent stack slot for signature help. Iced diffs Stack
        // children positionally; without this placeholder, a late signature
        // response shifts the completion scrollable to a new tree position
        // and discards its live scroll state.
        let signature_layer_index = editor_layers.len();
        editor_layers.push(
            iced::widget::container(iced::widget::space::vertical())
                .width(iced::Length::Fill)
                .height(height)
                .into(),
        );

        if !editor.is_dialog_open()
            && let Some(signature) = &editor.results().signature_help
        {
            let metrics = editor.overlay_metrics();
            if signature_overlay_should_render(signature, editor.results().hover.as_ref(), metrics)
            {
                let desired_height = signature_desired_height(signature, metrics);
                let placement = editor
                    .results()
                    .completion
                    .as_ref()
                    .filter(|completion| !completion.result.items.is_empty())
                    .map(|completion| {
                        coordinated_signature_completion_placements(
                            signature.anchor,
                            desired_height,
                            completion.anchor,
                            completion_desired_height(completion.result.items.len()),
                            metrics,
                        )
                        .0
                    })
                    .unwrap_or_else(|| hover_placement(signature.anchor, metrics, desired_height));
                let target = SignatureOverlayTarget {
                    document_id,
                    mount_generation,
                    identity: signature.identity,
                };

                let active_parameter = signature.result.active_parameter.map(usize::from);
                let mut signature_spans: Vec<iced::widget::text::Span<'_, ()>> =
                    vec![iced::widget::text::Span::new(
                        signature.result.prefix.as_str(),
                    )];
                for (index, parameter) in signature.result.parameters.iter().enumerate() {
                    if index > 0 {
                        signature_spans.push(iced::widget::text::Span::new(
                            signature.result.separator.as_str(),
                        ));
                    }
                    let mut span = iced::widget::text::Span::new(parameter.label.as_str());
                    if active_parameter == Some(index) {
                        // Span colors cannot be theme closures in iced 0.14.
                        // Underlining inherits the current palette's readable
                        // text color while still marking the active parameter.
                        span = span.underline(true).padding([0.0, 2.0]);
                    }
                    signature_spans.push(span);
                }
                signature_spans.push(iced::widget::text::Span::new(
                    signature.result.suffix.as_str(),
                ));
                let signature_line: super::Elem<'_> = iced::widget::rich_text(signature_spans)
                    .font(crate::assets::fonts::GEIST_MONO_VF)
                    .size(11.0)
                    .width(iced::Length::Fill)
                    .wrapping(iced::widget::text::Wrapping::WordOrGlyph)
                    .into();
                let mut signature_row = iced::widget::Row::new()
                    .align_y(iced::alignment::Vertical::Center)
                    .spacing(8.0)
                    .push(signature_line);
                if signature.result.signature_count > 1 {
                    signature_row = signature_row.push(
                        iced::widget::text(format!(
                            "{}/{}",
                            signature.result.selected_signature.saturating_add(1),
                            signature.result.signature_count
                        ))
                        .size(9.0)
                        .style(super::common::muted),
                    );
                }

                let mut body = iced::widget::Column::new().spacing(6.0).push(signature_row);
                if let Some(documentation) = &signature.active_parameter_documentation {
                    body = body.push(
                        rich_markup_view(documentation)
                            .map(move |uri| super::Message::CodeSignatureLinkPressed(target, uri)),
                    );
                }
                if let Some(documentation) = &signature.documentation {
                    body = body.push(
                        rich_markup_view(documentation)
                            .map(move |uri| super::Message::CodeSignatureLinkPressed(target, uri)),
                    );
                }

                let scroll_height = (placement.height - 12.0).max(1.0);
                let scrolling: super::Elem<'_> = iced::widget::scrollable(body)
                    .id(signature.scroll_id.clone())
                    .height(iced::Length::Fixed(scroll_height))
                    .into();
                let scrolling = iced::widget::keyed_column([(signature.identity, scrolling)])
                    .width(iced::Length::Fill)
                    .height(iced::Length::Fixed(scroll_height));
                let card = iced::widget::container(scrolling)
                    .padding(6.0)
                    .width(iced::Length::Fixed(placement.width))
                    .height(iced::Length::Fixed(placement.height))
                    .style(crate::theme::builtins::container::tooltip);
                let card =
                    iced::widget::mouse_area(card).interaction(iced::mouse::Interaction::Idle);
                editor_layers[signature_layer_index] = iced::widget::container(
                    iced::widget::column![
                        iced::widget::space::vertical().height(iced::Length::Fixed(placement.y)),
                        iced::widget::row![
                            iced::widget::space::horizontal()
                                .width(iced::Length::Fixed(placement.x)),
                            card
                        ]
                    ]
                    .spacing(0.0),
                )
                .width(iced::Length::Fill)
                .height(height)
                .into();
            }
        }

        if !editor.is_dialog_open()
            && let Some(completion) = &editor.results().completion
            && !completion.result.items.is_empty()
            && anchor_is_visible(completion.anchor, editor.overlay_metrics())
        {
            let metrics = editor.overlay_metrics();
            let visible_rows = completion_visible_rows(
                completion,
                editor.results().signature_help.as_ref(),
                metrics,
            );
            let placement = completion_placement_with_signature(
                completion,
                editor.results().signature_help.as_ref(),
                metrics,
            );
            let item_count = completion.result.items.len();
            let first_visible = completion
                .first_visible
                .min(item_count.saturating_sub(visible_rows));
            let render_start = first_visible.saturating_sub(COMPLETION_OVERSCAN_ROWS);
            let render_end = first_visible
                .saturating_add(visible_rows)
                .saturating_add(COMPLETION_OVERSCAN_ROWS)
                .min(item_count);
            let mut candidates = iced::widget::Column::new().spacing(0.0);
            if render_start > 0 {
                candidates = candidates.push(iced::widget::space::vertical().height(
                    iced::Length::Fixed(render_start as f32 * COMPLETION_ROW_HEIGHT),
                ));
            }
            for (index, item) in completion
                .result
                .items
                .iter()
                .enumerate()
                .skip(render_start)
                .take(render_end.saturating_sub(render_start))
            {
                let label = concise_completion_text(&item.label, 60);
                let kind = completion_kind_label(item.kind);
                let selection = CompletionSelection {
                    document_id,
                    mount_generation,
                    identity: completion.identity,
                    index,
                };
                candidates = candidates.push(
                    iced::widget::button(
                        iced::widget::row![
                            iced::widget::text(label)
                                .font(crate::assets::fonts::GEIST_MONO_VF)
                                .size(11.0)
                                .wrapping(iced::widget::text::Wrapping::None)
                                .style(if item.deprecated {
                                    super::common::muted
                                } else {
                                    super::common::regular
                                })
                                .width(iced::Length::Fill),
                            iced::widget::container(
                                iced::widget::text(kind)
                                    .font(crate::assets::fonts::GEIST_MONO_VF)
                                    .size(9.0)
                                    .style(completion_kind_style(item.kind, item.deprecated)),
                            )
                            .width(iced::Length::Fixed(78.0))
                            .align_x(iced::alignment::Horizontal::Right),
                        ]
                        .spacing(8.0)
                        .align_y(iced::alignment::Vertical::Center),
                    )
                    .padding([3.0, 6.0])
                    .width(iced::Length::Fill)
                    .height(iced::Length::Fixed(COMPLETION_ROW_HEIGHT))
                    .style(completion_row_style(index == completion.selected))
                    .on_press(super::Message::ApplyCodeCompletion(selection)),
                );
            }
            if render_end < item_count {
                candidates =
                    candidates.push(iced::widget::space::vertical().height(iced::Length::Fixed(
                        item_count.saturating_sub(render_end) as f32 * COMPLETION_ROW_HEIGHT,
                    )));
            }
            let scroll_height = (placement.height - 8.0).max(1.0);
            let identity = completion.identity;
            let max_first = item_count.saturating_sub(visible_rows);
            let scrolling: super::Elem<'_> = iced::widget::scrollable(candidates)
                .id(completion.scroll_id.clone())
                .height(iced::Length::Fixed(scroll_height))
                .on_scroll(move |viewport| {
                    let first_visible = (viewport.absolute_offset().y / COMPLETION_ROW_HEIGHT)
                        .floor()
                        .max(0.0) as usize;
                    super::Message::CodeCompletionViewportChanged(CompletionViewportTarget {
                        document_id,
                        mount_generation,
                        identity,
                        first_visible: first_visible.min(max_first),
                    })
                })
                .into();
            let scrolling = iced::widget::keyed_column([(completion.identity, scrolling)])
                .width(iced::Length::Fill)
                .height(iced::Length::Fixed(scroll_height));
            let card = iced::widget::container(scrolling)
                .padding(4.0)
                .width(iced::Length::Fixed(placement.width))
                .height(iced::Length::Fixed(placement.height))
                .style(crate::theme::builtins::container::tooltip);
            let card = iced::widget::mouse_area(card).interaction(iced::mouse::Interaction::Idle);
            editor_layers.push(
                iced::widget::container(
                    iced::widget::column![
                        iced::widget::space::vertical().height(iced::Length::Fixed(placement.y)),
                        iced::widget::row![
                            iced::widget::space::horizontal()
                                .width(iced::Length::Fixed(placement.x)),
                            card
                        ]
                    ]
                    .spacing(0.0),
                )
                .width(iced::Length::Fill)
                .height(height)
                .into(),
            );
        } else if !editor.is_dialog_open()
            && let Some(hover) = &editor.results().hover
            && anchor_is_visible(hover.anchor, editor.overlay_metrics())
        {
            let metrics = editor.overlay_metrics();
            let available_width = 520.0_f32.min((metrics.viewport_width - 8.0).max(1.0));
            let chars_per_line =
                ((available_width - 24.0).max(1.0) / metrics.char_width.max(4.0)).floor() as usize;
            let lines = hover
                .presentation
                .estimated_lines(chars_per_line)
                .clamp(2, 16) as f32;
            let placement = hover_placement(
                hover.anchor,
                metrics,
                (28.0 + lines * 18.0).clamp(64.0, 320.0),
            );
            let target = HoverOverlayTarget {
                document_id,
                mount_generation,
                identity: hover.identity,
            };
            let body: super::Elem<'_> = rich_markup_view(&hover.presentation)
                .map(move |uri| super::Message::CodeHoverLinkPressed(target, uri));
            let scroll_height = (placement.height - 12.0).max(1.0);
            let scrolling: super::Elem<'_> = iced::widget::scrollable(body)
                .height(iced::Length::Fixed(scroll_height))
                .into();
            let scrolling = iced::widget::keyed_column([(hover.identity, scrolling)])
                .width(iced::Length::Fill)
                .height(iced::Length::Fixed(scroll_height));
            let card = iced::widget::container(scrolling)
                .padding(6.0)
                .width(iced::Length::Fixed(placement.width))
                .height(iced::Length::Fixed(placement.height))
                .style(crate::theme::builtins::container::tooltip);
            let card = iced::widget::mouse_area(card)
                .interaction(iced::mouse::Interaction::Idle)
                .on_enter(super::Message::CodeHoverOverlayEntered(target))
                .on_move(move |_| super::Message::CodeHoverOverlayEntered(target))
                .on_exit(super::Message::CodeHoverOverlayExited(target));
            editor_layers.push(
                iced::widget::container(
                    iced::widget::column![
                        iced::widget::space::vertical().height(iced::Length::Fixed(placement.y)),
                        iced::widget::row![
                            iced::widget::space::horizontal()
                                .width(iced::Length::Fixed(placement.x)),
                            card
                        ]
                    ]
                    .spacing(0.0),
                )
                .width(iced::Length::Fill)
                .height(height)
                .into(),
            );
        }

        let editor_area = iced::widget::mouse_area(
            iced::widget::stack(editor_layers)
                .width(iced::Length::Fill)
                .height(height),
        )
        .on_exit(super::Message::DismissCodeOverlays);
        let suggestion_button = iced::widget::button(
            iced::widget::text(crate::i18n::t!("automation-code-show-completions")).size(11.0),
        )
        .padding([2.0, 6.0])
        .style(crate::theme::builtins::button::subtle)
        .on_press_maybe(
            (editor.service_status() == ServiceStatus::Ready)
                .then_some(super::Message::TriggerCodeCompletion),
        );
        let status_row = iced::widget::row![
            iced::widget::text(status)
                .size(11.0)
                .style(super::common::muted),
            iced::widget::space::horizontal(),
            suggestion_button,
        ]
        .align_y(iced::alignment::Vertical::Center);
        let mut chrome = iced::widget::Column::new()
            .spacing(4.0)
            .push(editor_area)
            .push(status_row);

        let visible_diagnostics = editor.visible_diagnostics().take(4).collect::<Vec<_>>();
        if !visible_diagnostics.is_empty() {
            let mut problems = iced::widget::Column::new().spacing(2.0).push(
                iced::widget::text(crate::i18n::t!("automation-code-problems"))
                    .size(11.0)
                    .style(super::common::regular),
            );
            for diagnostic in visible_diagnostics {
                problems = problems.push(
                    iced::widget::text(format!(
                        "{}:{}  {}",
                        diagnostic.range.start.line.saturating_add(1),
                        diagnostic.range.start.character.saturating_add(1),
                        diagnostic.message
                    ))
                    .size(11.0)
                    .style(super::common::muted),
                );
            }
            chrome = chrome.push(problems);
        }

        // Enter/exit follow cursor motion, which child widgets do not capture,
        // so the window can tell whether a press landed in the editor region
        // (editor, overlays, status row, and problems) without depending on
        // message ordering.
        iced::widget::mouse_area(chrome)
            .on_enter(super::Message::CodeEditorPointerEntered)
            .on_exit(super::Message::CodeEditorPointerExited)
            .into()
    }

    /// Applies one upstream editor event and maps every follow-up task back to this window.
    pub(super) fn update_code_editor(
        &mut self,
        message: &BoundEditorMessage,
    ) -> (iced::Task<super::Message>, bool) {
        let Some(editor) = &mut self.code_editor else {
            return (iced::Task::none(), false);
        };
        if editor.document().document.key.document_id != message.document_id {
            return (iced::Task::none(), false);
        }
        if self.code_editor_mount_generation != message.mount_generation {
            return (iced::Task::none(), false);
        }
        if editor
            .results
            .completion
            .as_ref()
            .is_some_and(|completion| {
                anchor_is_visible(completion.anchor, editor.overlay_metrics())
            })
        {
            let visible_rows = editor
                .results
                .completion
                .as_ref()
                .map(|completion| {
                    completion_visible_rows(
                        completion,
                        editor.results.signature_help.as_ref(),
                        editor.overlay_metrics(),
                    )
                })
                .unwrap_or(1);
            let navigation = match message.message {
                IcedEditorMessage::ArrowKey(iced_code_editor::ArrowDirection::Up, false) => {
                    Some(CompletionNavigation::Previous)
                }
                IcedEditorMessage::ArrowKey(iced_code_editor::ArrowDirection::Down, false) => {
                    Some(CompletionNavigation::Next)
                }
                IcedEditorMessage::PageUp(false) => Some(CompletionNavigation::PageUp),
                IcedEditorMessage::PageDown(false) => Some(CompletionNavigation::PageDown),
                IcedEditorMessage::Home(false) => Some(CompletionNavigation::First),
                IcedEditorMessage::End(false) => Some(CompletionNavigation::Last),
                _ => None,
            };
            if let Some(update) = navigation
                .and_then(|navigation| editor.navigate_completion(navigation, visible_rows))
            {
                debug_assert!(update.selected < update.count);
                let task = update.scroll.map_or_else(iced::Task::none, |(id, first)| {
                    completion_scroll_task(id, first)
                });
                return (task, false);
            }
            if matches!(message.message, IcedEditorMessage::Enter)
                && let Some((task, changed)) = editor.apply_selected_completion()
            {
                let document_id = editor.document().document.key.document_id;
                let mount_generation = self.code_editor_mount_generation;
                return (
                    task.map(move |message| {
                        super::Message::CodeEditorAction(BoundEditorMessage {
                            document_id,
                            mount_generation,
                            message,
                        })
                    }),
                    changed,
                );
            }
        }
        let (task, changed) = editor.update_with_change(&message.message);
        if editor.service_status() == ServiceStatus::Ready && editor.service_state.is_some() {
            if editor.pending_completion.is_some() {
                let request = next_wire_value::<RequestId>(&mut self.next_language_request_id);
                let _ = editor.request_pending_completion(request);
            }
            if editor.pending_signature_help.is_some() {
                let request = next_wire_value::<RequestId>(&mut self.next_language_request_id);
                let _ = editor.request_pending_signature_help(request);
            }
            if editor.pending_definition.is_some() {
                let request = next_wire_value::<RequestId>(&mut self.next_language_request_id);
                let _ = editor.request_pending_definition(request);
            }
            if editor.pending_formatting.is_some() {
                let request = next_wire_value::<RequestId>(&mut self.next_language_request_id);
                let _ = editor.request_pending_formatting(request);
            }
        }
        let document_id = editor.document().document.key.document_id;
        let mount_generation = self.code_editor_mount_generation;
        (
            task.map(move |message| {
                super::Message::CodeEditorAction(BoundEditorMessage {
                    document_id,
                    mount_generation,
                    message,
                })
            }),
            changed,
        )
    }

    /// Tracks the scroll position of one exact completion result so subsequent
    /// keyboard navigation only reveals a row after it leaves the viewport.
    pub(super) fn code_completion_viewport_changed(&mut self, target: CompletionViewportTarget) {
        let Some(editor) = &mut self.code_editor else {
            return;
        };
        if target.document_id != editor.document().document.key.document_id
            || target.mount_generation != self.code_editor_mount_generation
        {
            return;
        }
        let _ = editor.completion_viewport_changed(target.identity, target.first_visible);
    }

    /// Applies a completion selected from the visible candidate list.
    pub(super) fn apply_code_completion(
        &mut self,
        selection: CompletionSelection,
    ) -> (iced::Task<super::Message>, bool) {
        let Some(editor) = &mut self.code_editor else {
            return (iced::Task::none(), false);
        };
        if selection.document_id != editor.document().document.key.document_id
            || selection.mount_generation != self.code_editor_mount_generation
        {
            return (iced::Task::none(), false);
        }
        let Some((task, changed)) = editor.apply_completion(selection.index, selection.identity)
        else {
            return (iced::Task::none(), false);
        };
        let document_id = editor.document().document.key.document_id;
        let mount_generation = self.code_editor_mount_generation;
        (
            task.map(move |message| {
                super::Message::CodeEditorAction(BoundEditorMessage {
                    document_id,
                    mount_generation,
                    message,
                })
            }),
            changed,
        )
    }

    pub(super) fn code_hover_overlay_entered(&mut self, target: HoverOverlayTarget) {
        let Some(editor) = &mut self.code_editor else {
            return;
        };
        if target.document_id != editor.document().document.key.document_id
            || target.mount_generation != self.code_editor_mount_generation
        {
            return;
        }
        let _ = editor.hover_overlay_entered(target.identity);
    }

    pub(super) fn code_hover_overlay_exited(&mut self, target: HoverOverlayTarget) {
        let Some(editor) = &mut self.code_editor else {
            return;
        };
        if target.document_id != editor.document().document.key.document_id
            || target.mount_generation != self.code_editor_mount_generation
        {
            return;
        }
        let _ = editor.hover_overlay_exited(target.identity, Instant::now());
    }

    pub(super) fn code_signature_link_pressed(&self, target: SignatureOverlayTarget) {
        let Some(editor) = &self.code_editor else {
            return;
        };
        if target.document_id != editor.document().document.key.document_id
            || target.mount_generation != self.code_editor_mount_generation
        {
            return;
        }
        let _ = editor.signature_help_is_current(target.identity);
    }

    /// Closes the active document while retaining the resident window-scoped worker.
    pub(super) fn clear_code_editor(&mut self) {
        self.code_editor = None;
        self.pointer_over_code_editor = false;
    }

    /// Takes keyboard focus away from the code editor because another widget
    /// is about to receive it. The upstream canvas would otherwise keep
    /// inserting every keystroke typed into that widget.
    pub(super) fn release_code_editor_focus(&mut self) {
        if let Some(editor) = &mut self.code_editor {
            editor.lose_focus();
        }
    }

    /// Records a successful durable save in both editor history and the service.
    pub(super) fn mark_code_editor_saved(&mut self) {
        if self.code_editor.is_none() {
            return;
        }
        let revision = next_wire_value::<DiskRevision>(&mut self.next_code_disk_revision);
        if let Some(editor) = &mut self.code_editor {
            editor.mark_saved(revision);
        }
    }

    pub(super) fn code_editor_is_modified(&self) -> bool {
        self.code_editor
            .as_ref()
            .is_some_and(AutomationCodeEditor::is_modified)
    }

    fn bind_code_editor_task(
        &self,
        task: iced::Task<IcedEditorMessage>,
    ) -> iced::Task<super::Message> {
        let Some(document_id) = self
            .code_editor
            .as_ref()
            .map(|editor| editor.document().document.key.document_id)
        else {
            return iced::Task::none();
        };
        let mount_generation = self.code_editor_mount_generation;
        task.map(move |message| {
            super::Message::CodeEditorAction(BoundEditorMessage {
                document_id,
                mount_generation,
                message,
            })
        })
    }

    /// Drains worker events without blocking the UI, applies a fenced format reply, and
    /// returns any one-target definition navigation task.
    pub(super) fn poll_language_service(&mut self) -> (iced::Task<super::Message>, bool) {
        let events = self
            .language_service
            .as_mut()
            .map(LanguageServiceHost::drain_events)
            .unwrap_or_default();
        let mut project_retry = None;
        for event in &events {
            if let Some(retry) = self.observe_language_project_event(event) {
                project_retry = Some(retry);
            }
        }
        let mut refresh_diagnostics = false;
        let mut accepted_definition = None;
        let mut service_edit_applied = false;
        if let Some(editor) = &mut self.code_editor {
            for event in &events {
                let disposition = editor.apply_service_event(event);
                refresh_diagnostics |= disposition == EventDisposition::Applied
                    && matches!(
                        event.event,
                        Event::StateAcknowledged(
                            AcknowledgedState::DocumentOpened(_)
                                | AcknowledgedState::DocumentChanged(_)
                                | AcknowledgedState::DocumentSaved(_)
                                | AcknowledgedState::ProjectRefreshed(_)
                        )
                    );
            }
            let _ = editor.expire_hover_dismiss(Instant::now());
            editor.retry_service_sync();
            accepted_definition = editor.take_definition();
            service_edit_applied = editor.take_service_edit_applied();
        }
        if let Some(retry) = project_retry
            && let Some(client) = self
                .language_service
                .as_ref()
                .map(LanguageServiceHost::client)
        {
            self.install_language_project_with_retries(
                retry.context,
                &client,
                retry.retries_remaining,
            );
        }
        if refresh_diagnostics && let Some(editor) = &mut self.code_editor {
            let request = next_wire_value::<RequestId>(&mut self.next_language_request_id);
            let _ = editor.request_diagnostics(request);
        }
        if let Some(editor) = &mut self.code_editor
            && editor.service_status() == ServiceStatus::Ready
            && editor.service_state.is_some()
        {
            if editor.pending_completion.is_some() {
                let request = next_wire_value::<RequestId>(&mut self.next_language_request_id);
                let _ = editor.request_pending_completion(request);
            }
            if editor.pending_signature_help.is_some() {
                let request = next_wire_value::<RequestId>(&mut self.next_language_request_id);
                let _ = editor.request_pending_signature_help(request);
            }
            let now = Instant::now();
            if editor
                .pending_hover
                .as_ref()
                .is_some_and(|pending| now >= pending.ready_at)
            {
                let request = next_wire_value::<RequestId>(&mut self.next_language_request_id);
                let _ = editor.request_pending_hover(request, now);
            }
            if editor.pending_definition.is_some() {
                let request = next_wire_value::<RequestId>(&mut self.next_language_request_id);
                let _ = editor.request_pending_definition(request);
            }
            if editor.pending_formatting.is_some() {
                let request = next_wire_value::<RequestId>(&mut self.next_language_request_id);
                let _ = editor.request_pending_formatting(request);
            }
        }
        let task = accepted_definition.map_or_else(iced::Task::none, |definition| {
            self.definition_navigation_task(definition)
        });
        (task, service_edit_applied)
    }

    fn definition_navigation_task(
        &mut self,
        definition: AcceptedDefinition,
    ) -> iced::Task<super::Message> {
        let Some(editor) = self.code_editor.as_ref() else {
            return iced::Task::none();
        };
        if !editor.is_current_state(definition.origin) {
            return iced::Task::none();
        }
        let current_document = editor.document().document.key.document_id;
        let target = definition.result.targets.into_iter().find(|target| {
            target.document_id == current_document || self.definition_source_key(target).is_some()
        });
        let Some(target) = target else {
            return iced::Task::none();
        };

        if target.document_id == current_document {
            let Some(editor) = &mut self.code_editor else {
                return iced::Task::none();
            };
            let Some(task) = editor.goto_utf16_position(target.target_selection_range.start) else {
                return iced::Task::none();
            };
            let document_id = editor.document().document.key.document_id;
            let mount_generation = self.code_editor_mount_generation;
            return task.map(move |message| {
                super::Message::CodeEditorAction(BoundEditorMessage {
                    document_id,
                    mount_generation,
                    message,
                })
            });
        }

        iced::Task::done(super::Message::NavigateCodeDefinition(
            DefinitionNavigation {
                origin: definition.origin,
                origin_mount_generation: self.code_editor_mount_generation,
                target,
            },
        ))
    }

    fn definition_source_key(&self, target: &DefinitionTarget) -> Option<LanguageSourceKey> {
        let key = self
            .language_source_ids
            .iter()
            .find_map(|(key, document_id)| {
                (*document_id == target.document_id).then(|| key.clone())
            })?;
        if matches!(key, LanguageSourceKey::InlineBridge)
            || !self.language_source_key_is_current(&key)
        {
            return None;
        }
        let expected_uri = language_source_key_uri(&key)?;
        target
            .analyzed_uri
            .as_deref()
            .is_none_or(|uri| uri == expected_uri)
            .then_some(key)
    }

    fn language_source_key_is_current(&self, key: &LanguageSourceKey) -> bool {
        match (&self.language_project_context, key) {
            (Some(LanguageProjectContext::Modules), LanguageSourceKey::Module(_)) => true,
            (
                Some(LanguageProjectContext::OwnedPackage(current)),
                LanguageSourceKey::OwnedPackage { package, .. },
            ) => current == package,
            (Some(LanguageProjectContext::Inline), LanguageSourceKey::InlineAutomation { .. }) => {
                true
            }
            _ => false,
        }
    }

    pub(super) fn navigate_code_definition(
        &mut self,
        navigation: DefinitionNavigation,
    ) -> crate::update::Update<super::Message, super::Event> {
        self.navigate_code_definition_checked(navigation).0
    }

    /// Performs a definition jump and reports whether the origin draft was actually left.
    /// The outcome lets the Discard confirmation preserve its dirty state when a queued jump
    /// became stale while the confirmation banner was open.
    pub(super) fn navigate_code_definition_checked(
        &mut self,
        navigation: DefinitionNavigation,
    ) -> (crate::update::Update<super::Message, super::Event>, bool) {
        let Some(editor) = self.code_editor.as_ref() else {
            return (crate::update::Update::none(), false);
        };
        if self.code_editor_mount_generation != navigation.origin_mount_generation
            || !editor.is_current_state(navigation.origin)
        {
            return (crate::update::Update::none(), false);
        }
        let origin_document_id = editor.document().document.key.document_id;
        let origin_mount_generation = self.code_editor_mount_generation;
        let Some(key) = self.definition_source_key(&navigation.target) else {
            return (crate::update::Update::none(), false);
        };
        let mut update = match key {
            LanguageSourceKey::Module(subpath) => self.open_module(subpath),
            LanguageSourceKey::OwnedPackage { package, subpath }
                if self
                    .local_package
                    .as_ref()
                    .is_some_and(|open| open.name == package) =>
            {
                self.select_owned_file(subpath)
            }
            LanguageSourceKey::OwnedPackage { .. } | LanguageSourceKey::InlineBridge => {
                return (crate::update::Update::none(), false);
            }
            LanguageSourceKey::InlineAutomation {
                folder_name,
                script_name,
                ..
            } => self.open_script(super::model::ScriptKey {
                folder_name,
                script_name,
            }),
        };
        let left_origin = !self.code_editor.as_ref().is_some_and(|editor| {
            editor.document().document.key.document_id == origin_document_id
                && self.code_editor_mount_generation == origin_mount_generation
        });
        let Some(editor) = &mut self.code_editor else {
            return (update, left_origin);
        };
        if editor.document().document.key.document_id != navigation.target.document_id {
            return (update, left_origin);
        }
        if !definition_target_ranges_fit(&navigation.target, &editor.content()) {
            return (update, true);
        }
        let Some(task) = editor.goto_utf16_position(navigation.target.target_selection_range.start)
        else {
            return (update, true);
        };
        let document_id = editor.document().document.key.document_id;
        let mount_generation = self.code_editor_mount_generation;
        update.task = iced::Task::batch([
            update.task,
            task.map(move |message| {
                super::Message::CodeEditorAction(BoundEditorMessage {
                    document_id,
                    mount_generation,
                    message,
                })
            }),
        ]);
        (update, true)
    }

    /// Replaces the saved-source base for the current authoring context.
    ///
    /// The mounted editor remains open and therefore continues to shadow its matching
    /// saved source until it is closed or rebound.
    pub(super) fn refresh_language_project(&mut self) {
        let (Some(context), Some(client)) = (
            self.language_project_target_context
                .clone()
                .or_else(|| self.language_project_context.clone()),
            self.language_service
                .as_ref()
                .map(LanguageServiceHost::client),
        ) else {
            return;
        };
        self.install_language_project(context, &client);
    }

    /// The inline bridge for the open state draft: the fixed inline declarations, reshaped per
    /// automation so each exposed name is declared and the user-API member it shadows is not.
    fn inline_bridge_text(&self) -> String {
        let exposures = self
            .state_exposures
            .iter()
            .map(super::state_values::StateExposureDraft::to_exposure)
            .collect::<Vec<_>>();
        smudgy_core::models::script_typings::language_service_inline_bridge_for(&exposures)
            .into_owned()
    }

    /// The inline bridge the worker holds or is installing: the in-flight refresh's while one
    /// is pending (its acknowledgement records it, and a retry is rebuilt from the draft), else
    /// the newest acknowledged one; `None` while that project is not the inline one.
    fn installed_inline_bridge(&self) -> Option<&str> {
        self.pending_language_project_refresh.as_ref().map_or_else(
            || self.inline_bridge.as_deref(),
            |pending| pending.inline_bridge.as_deref(),
        )
    }

    /// Whether the open draft's bridge differs from the installed or in-flight one.
    pub(super) fn inline_bridge_is_stale(&self) -> bool {
        self.installed_inline_bridge() != Some(self.inline_bridge_text().as_str())
    }

    /// Re-installs the inline project after an exposure edit changed the open draft's bridge,
    /// so the body's diagnostics and completions follow the names it can use. Nothing happens
    /// while another context is targeted, or while the installed or in-flight bridge reads the
    /// same.
    pub(super) fn refresh_inline_bridge(&mut self) {
        if !self.language_project_context_matches(&LanguageProjectContext::Inline)
            || !self.inline_bridge_is_stale()
        {
            return;
        }
        self.language_project_target_context = Some(LanguageProjectContext::Inline);
        self.refresh_language_project();
    }

    fn install_language_project(
        &mut self,
        context: LanguageProjectContext,
        client: &LanguageServiceClient,
    ) {
        self.install_language_project_with_retries(context, client, 1);
    }

    fn install_language_project_with_retries(
        &mut self,
        context: LanguageProjectContext,
        client: &LanguageServiceClient,
        retries_remaining: u8,
    ) {
        let LanguageProjectSources {
            sources,
            inline_bridge,
        } = self.language_project_sources(&context);
        let graph_generation =
            next_wire_value::<GraphGeneration>(&mut self.next_language_graph_generation);
        match client.send(Command::RefreshProject(RefreshProject {
            project: language_service_project(),
            graph_generation,
            sources,
        })) {
            Ok(command_sequence) => {
                self.pending_language_project_refresh = Some(PendingLanguageProjectRefresh {
                    context,
                    graph_generation,
                    command_sequence,
                    retries_remaining,
                    inline_bridge,
                });
            }
            Err(error) => {
                // The installed bridge is unchanged, so an inline draft still reads as stale
                // and the next editor binding or exposure edit sends the refresh again.
                log::warn!("Failed to refresh Automations language-service project: {error}");
            }
        }
    }

    fn observe_language_project_event(
        &mut self,
        envelope: &EventEnvelope,
    ) -> Option<ProjectRefreshRetry> {
        if envelope.validate().is_err()
            || self
                .pending_language_project_refresh
                .as_ref()
                .is_none_or(|pending| pending.command_sequence != envelope.command_sequence)
        {
            return None;
        }
        match &envelope.event {
            Event::StateAcknowledged(AcknowledgedState::ProjectRefreshed(project)) => {
                let pending = self.pending_language_project_refresh.as_ref()?;
                if project.project != language_service_project()
                    || project.graph_generation != pending.graph_generation
                {
                    return None;
                }
                let pending = self.pending_language_project_refresh.take()?;
                self.language_project_context = Some(pending.context);
                self.inline_bridge = pending.inline_bridge;
                None
            }
            Event::RequestFailed(failure) => {
                // The refresh installed nothing: the acknowledged bridge stands, and a draft
                // whose bridge differs from it reads as stale until a later refresh carries it.
                let pending = self.pending_language_project_refresh.take()?;
                (failure.retryable
                    && pending.retries_remaining > 0
                    && self.language_project_target_context.as_ref() == Some(&pending.context))
                .then(|| ProjectRefreshRetry {
                    context: pending.context,
                    retries_remaining: pending.retries_remaining - 1,
                })
            }
            _ => None,
        }
    }

    fn language_project_is_installed_or_pending(&self, context: &LanguageProjectContext) -> bool {
        self.pending_language_project_refresh.as_ref().map_or_else(
            || self.language_project_context.as_ref() == Some(context),
            |pending| &pending.context == context,
        )
    }

    pub(super) fn language_project_context_matches(
        &self,
        context: &LanguageProjectContext,
    ) -> bool {
        self.language_project_target_context.as_ref().map_or_else(
            || {
                self.pending_language_project_refresh.as_ref().map_or_else(
                    || self.language_project_context.as_ref() == Some(context),
                    |pending| &pending.context == context,
                )
            },
            |target| target == context,
        )
    }

    /// Reconciles a newly loaded standalone-module inventory with the mounted editor.
    ///
    /// A dirty editor stays mounted as the authoritative overlay. A clean editor is closed
    /// before the refreshed base is queued and then reopened from disk, preserving the worker's
    /// close -> refresh -> open ordering and preventing a stale clean overlay from hiding reloads.
    pub(super) fn reconcile_module_language_project_reload(
        &mut self,
    ) -> iced::Task<super::Message> {
        let clean = !self.dirty && !self.code_editor_is_modified();
        let open_subpath = match &self.pane {
            super::Pane::Module(super::ModuleState {
                subpath,
                path: Some(_),
                ..
            }) if clean => Some(subpath.clone()),
            _ => None,
        };
        let reloaded = open_subpath.as_ref().map(|subpath| {
            let module = self
                .modules
                .iter()
                .find(|module| &module.subpath == subpath)
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "module disappeared during reload",
                    )
                })?;
            let text = std::fs::read_to_string(&module.path)?;
            Ok::<_, std::io::Error>((module.path.clone(), text, path_language(subpath)))
        });

        if reloaded.is_some() {
            self.clear_code_editor();
        }
        if self.language_project_context_matches(&LanguageProjectContext::Modules) {
            self.language_project_target_context = Some(LanguageProjectContext::Modules);
            self.refresh_language_project();
        }

        match reloaded {
            Some(Ok((path, text, language))) => {
                if let super::Pane::Module(state) = &mut self.pane {
                    state.path = Some(path);
                }
                self.module_source_baseline = Some(text.clone());
                self.bind_code_editor(&text, language, CodeDocument::StandaloneModule)
            }
            Some(Err(error)) => {
                let subpath = open_subpath.unwrap_or_default();
                self.pane = super::Pane::Error(std::sync::Arc::new(vec![crate::i18n::t!(
                    "editor-failed-read",
                    "path" => subpath,
                    "error" => error.to_string()
                )]));
                iced::Task::none()
            }
            None => iced::Task::none(),
        }
    }

    /// Reloads the package currently shown in the owned-package pane and reconciles its graph.
    /// Dirty source text remains mounted over the new disk base; clean text is remounted from the
    /// package snapshot. Binary/unreadable files retain the existing read-error degradation.
    pub(super) fn reconcile_owned_package_language_project_reload(
        &mut self,
    ) -> iced::Task<super::Message> {
        let Some(name) = (match &self.selection {
            super::Selection::OwnedPackage(name) => Some(name.clone()),
            _ => None,
        }) else {
            return iced::Task::none();
        };
        let package =
            match smudgy_core::models::local_packages::load_local_package(&self.server_name, &name)
            {
                Ok(Some(package)) => package,
                Ok(None) => return iced::Task::none(),
                Err(error) => {
                    log::warn!("Failed to reload open local package {name}: {error}");
                    return iced::Task::none();
                }
            };

        let clean = !self.dirty && !self.code_editor_is_modified();
        let selected = self.owned_selected_file.clone();
        let reloaded = selected.as_ref().filter(|_| clean).map(|subpath| {
            if subpath.eq_ignore_ascii_case("README.md") {
                return package
                    .readme
                    .clone()
                    .ok_or("file disappeared during reload");
            }
            let module = package
                .modules
                .iter()
                .find(|module| &module.subpath == subpath)
                .ok_or("file disappeared during reload")?;
            String::from_utf8(module.content.clone()).map_err(|_| "file is not UTF-8 text")
        });

        if reloaded.is_some() {
            self.clear_code_editor();
        }
        self.local_readme = package
            .readme
            .as_deref()
            .map(iced::widget::markdown::Content::parse);
        self.local_package = Some(Box::new(package));
        let context = LanguageProjectContext::OwnedPackage(name.clone());
        if self.language_project_context_matches(&context) {
            self.language_project_target_context = Some(context);
            self.refresh_language_project();
        }

        match (selected, reloaded) {
            (Some(subpath), Some(Ok(text))) => {
                self.owned_source_baseline = Some(text.clone());
                self.bind_code_editor(&text, path_language(&subpath), CodeDocument::OwnedPackage)
            }
            (Some(subpath), Some(Err(error))) => {
                self.owned_selected_file = None;
                self.authoring_feedback = Some(crate::i18n::t!(
                    "package-file-read-failed",
                    "path" => &subpath,
                    "error" => error
                ));
                iced::Task::none()
            }
            _ => iced::Task::none(),
        }
    }

    fn language_project_sources(
        &mut self,
        context: &LanguageProjectContext,
    ) -> LanguageProjectSources {
        let mut pending = Vec::new();
        let mut total_bytes = 0_usize;
        let mut inline_bridge = None;
        match context {
            // Inline automations are isolated lexical bodies in one shared main-isolate global
            // context. Install every saved inline body so globalThis and inferred vars state can
            // cross aliases/triggers/hotkeys, while keeping the graph separate from modules and
            // packages. Absolute ambient modules such as node: and smudgy: still come from the
            // managed declarations. The bridge is the open automation's: its state exposures
            // are declared as the names its body can use, in place of the user-API members
            // they shadow. It counts as installed once the refresh carrying it is acknowledged.
            LanguageProjectContext::Inline => {
                let bridge = self.inline_bridge_text();
                inline_bridge = Some(bridge.clone());
                total_bytes = bridge.len();
                pending.push(PendingProjectSource {
                    key: LanguageSourceKey::InlineBridge,
                    uri: "smudgy-project:///inline/context.d.ts".to_owned(),
                    language: Language::TypeScript,
                    kind: DocumentKind::Generated,
                    text: bridge,
                });
                collect_inline_project_sources(&self.scripts, None, &mut pending, &mut total_bytes);
            }
            LanguageProjectContext::Modules => {
                for module in &self.modules {
                    if pending.len() == MAX_PROJECT_SOURCE_FILES {
                        break;
                    }
                    let Some(uri) = project_source_uri("modules", &module.subpath) else {
                        continue;
                    };
                    let language = path_language(&module.subpath);
                    if !supports_project_source(language) {
                        continue;
                    }
                    let Ok(metadata) = std::fs::symlink_metadata(&module.path) else {
                        continue;
                    };
                    let remaining = MAX_PROJECT_SOURCE_TEXT_BYTES.saturating_sub(total_bytes);
                    let admitted_bytes = MAX_DOCUMENT_BYTES.min(remaining);
                    if !metadata.is_file()
                        || usize::try_from(metadata.len())
                            .map_or(true, |length| length > admitted_bytes)
                    {
                        continue;
                    }
                    let Ok(file) = std::fs::File::open(&module.path) else {
                        continue;
                    };
                    let Ok(limit) = u64::try_from(admitted_bytes.saturating_add(1)) else {
                        continue;
                    };
                    let mut bytes = Vec::with_capacity(
                        usize::try_from(metadata.len()).unwrap_or(admitted_bytes),
                    );
                    if file.take(limit).read_to_end(&mut bytes).is_err()
                        || bytes.len() > admitted_bytes
                    {
                        continue;
                    }
                    let Ok(text) = String::from_utf8(bytes) else {
                        continue;
                    };
                    if validate_document_text(&text).is_err() {
                        continue;
                    };
                    total_bytes += text.len();
                    pending.push(PendingProjectSource {
                        key: LanguageSourceKey::Module(module.subpath.clone()),
                        uri,
                        language,
                        kind: DocumentKind::Dependency,
                        text,
                    });
                }
            }
            LanguageProjectContext::OwnedPackage(name) => {
                let modules = self
                    .local_package
                    .as_deref()
                    .filter(|package| package.name == *name)
                    .map(|package| package.modules.as_slice())
                    .unwrap_or_default();
                for module in modules {
                    if pending.len() == MAX_PROJECT_SOURCE_FILES {
                        break;
                    }
                    let Some(uri) = owned_package_source_uri(name, &module.subpath) else {
                        continue;
                    };
                    let language = path_language(&module.subpath);
                    if !supports_project_source(language) {
                        continue;
                    }
                    let remaining = MAX_PROJECT_SOURCE_TEXT_BYTES.saturating_sub(total_bytes);
                    let admitted_bytes = MAX_DOCUMENT_BYTES.min(remaining);
                    if module.content.len() > admitted_bytes {
                        continue;
                    }
                    let Ok(text) = std::str::from_utf8(&module.content) else {
                        continue;
                    };
                    if validate_document_text(text).is_err() {
                        continue;
                    }
                    total_bytes += text.len();
                    pending.push(PendingProjectSource {
                        key: LanguageSourceKey::OwnedPackage {
                            package: name.clone(),
                            subpath: module.subpath.clone(),
                        },
                        uri,
                        language,
                        kind: DocumentKind::Dependency,
                        text: text.to_owned(),
                    });
                }
            }
        }

        let mut sources = Vec::with_capacity(pending.len());
        for source in pending {
            let document_id = self.language_source_id(source.key);
            sources.push(ProjectSource {
                document_id,
                uri: source.uri,
                language: source.language,
                kind: source.kind,
                text: source.text,
            });
        }
        LanguageProjectSources {
            sources,
            inline_bridge,
        }
    }

    fn code_document_descriptor(
        &mut self,
        language: Language,
        kind: CodeDocument,
    ) -> DocumentDescriptor {
        let logical = logical_editor_source(self, language, kind);
        let (document_id, uri) = logical.map_or_else(
            || {
                let document_id = allocate_document_id();
                let extension = language_extension(language);
                (
                    document_id,
                    format!("smudgy-authoring:///document/{document_id}.{extension}"),
                )
            },
            |(key, uri)| (self.language_source_id(key), uri),
        );
        document_descriptor(document_id, uri, language, kind)
    }

    fn language_source_id(&mut self, key: LanguageSourceKey) -> DocumentId {
        *self
            .language_source_ids
            .entry(key)
            .or_insert_with(allocate_document_id)
    }

    fn ensure_language_service(&mut self) -> Option<LanguageServiceClient> {
        if let Some(host) = &self.language_service {
            return Some(host.client());
        }

        let libraries = smudgy_core::models::script_typings::embedded_language_service_types()
            .into_iter()
            .map(|file| LanguageServiceLibrary {
                file_name: file.virtual_path,
                text: file.contents.into(),
                is_root: file.is_root,
            })
            .collect();
        let host = match LanguageServiceHost::try_spawn_with_libraries(libraries) {
            Ok(host) => host,
            Err(error) => {
                log::warn!("Failed to start Automations language service: {error}");
                return None;
            }
        };
        let client = host.client();
        if let Err(error) = client.send(Command::OpenProject(OpenProject {
            project: language_service_project(),
        })) {
            log::warn!("Failed to open Automations language-service project: {error}");
        }
        self.language_service = Some(host);
        Some(client)
    }
}

/// Maps a persisted script language to an authoring/highlighting language.
pub(super) const fn script_language(language: smudgy_core::models::ScriptLang) -> Language {
    match language {
        smudgy_core::models::ScriptLang::JS => Language::JavaScript,
        smudgy_core::models::ScriptLang::TS => Language::TypeScript,
        smudgy_core::models::ScriptLang::Plaintext => Language::PlainText,
    }
}

pub(super) const fn supports_language_service(language: Language) -> bool {
    matches!(
        language,
        Language::JavaScript
            | Language::TypeScript
            | Language::JavaScriptReact
            | Language::TypeScriptReact
    )
}

/// Chooses a JS/TS-family language from a writable module path.
pub(super) fn path_language(path: &str) -> Language {
    match std::path::Path::new(path)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("js" | "mjs" | "cjs") => Language::JavaScript,
        Some("ts" | "mts" | "cts") => Language::TypeScript,
        Some("jsx") => Language::JavaScriptReact,
        Some("tsx") => Language::TypeScriptReact,
        Some("json") => Language::Json,
        _ => Language::PlainText,
    }
}

fn language_service_project() -> ProjectScope {
    ProjectScope {
        client_id: ClientId::new(1).expect("one is a valid language-service client id"),
        project_id: ProjectId::new(1).expect("one is a valid language-service project id"),
    }
}

fn language_project_context(
    window: &super::AutomationsWindow,
    kind: CodeDocument,
) -> LanguageProjectContext {
    match kind {
        CodeDocument::Alias | CodeDocument::Trigger | CodeDocument::Hotkey => {
            LanguageProjectContext::Inline
        }
        CodeDocument::StandaloneModule => LanguageProjectContext::Modules,
        CodeDocument::OwnedPackage => window.local_package.as_ref().map_or_else(
            || LanguageProjectContext::OwnedPackage(String::new()),
            |package| LanguageProjectContext::OwnedPackage(package.name.clone()),
        ),
    }
}

fn logical_editor_source(
    window: &super::AutomationsWindow,
    language: Language,
    kind: CodeDocument,
) -> Option<(LanguageSourceKey, String)> {
    match kind {
        CodeDocument::StandaloneModule => {
            let super::Pane::Module(state) = &window.pane else {
                return None;
            };
            state.path.as_ref()?;
            let uri = project_source_uri("modules", &state.subpath)?;
            Some((LanguageSourceKey::Module(state.subpath.clone()), uri))
        }
        CodeDocument::OwnedPackage => {
            let package = window.local_package.as_ref()?.name.clone();
            let subpath = window.owned_selected_file.clone()?;
            let uri = owned_package_source_uri(&package, &subpath)?;
            Some((LanguageSourceKey::OwnedPackage { package, subpath }, uri))
        }
        CodeDocument::Alias | CodeDocument::Trigger | CodeDocument::Hotkey => {
            let super::Selection::Script(key) = &window.selection else {
                return None;
            };
            let source_key = LanguageSourceKey::InlineAutomation {
                kind,
                language,
                folder_name: key.folder_name.clone(),
                script_name: key.script_name.clone(),
            };
            let uri = language_source_key_uri(&source_key)?;
            Some((source_key, uri))
        }
    }
}

fn project_source_uri(namespace: &str, subpath: &str) -> Option<String> {
    valid_project_subpath(subpath).then(|| format!("smudgy-project:///{namespace}/{subpath}"))
}

fn owned_package_source_uri(package: &str, subpath: &str) -> Option<String> {
    if !valid_project_subpath(package) {
        return None;
    }
    project_source_uri(&format!("packages/{package}"), subpath)
}

fn language_source_key_uri(key: &LanguageSourceKey) -> Option<String> {
    match key {
        LanguageSourceKey::Module(subpath) => project_source_uri("modules", subpath),
        LanguageSourceKey::OwnedPackage { package, subpath } => {
            owned_package_source_uri(package, subpath)
        }
        LanguageSourceKey::InlineAutomation {
            kind,
            language,
            folder_name,
            script_name,
        } => inline_automation_source_uri(*kind, *language, folder_name.as_deref(), script_name),
        LanguageSourceKey::InlineBridge => Some("smudgy-project:///inline/context.d.ts".to_owned()),
    }
}

fn inline_automation_source_uri(
    kind: CodeDocument,
    language: Language,
    folder_name: Option<&str>,
    script_name: &str,
) -> Option<String> {
    let kind = match kind {
        CodeDocument::Alias => "aliases",
        CodeDocument::Trigger => "triggers",
        CodeDocument::Hotkey => "hotkeys",
        CodeDocument::StandaloneModule | CodeDocument::OwnedPackage => return None,
    };
    let extension = language_extension(language);
    let mut uri = url::Url::parse("smudgy-project:///").ok()?;
    let mut segments = uri.path_segments_mut().ok()?;
    segments.push("inline").push(kind);
    if let Some(folder_name) = folder_name.filter(|folder| !folder.is_empty()) {
        for segment in folder_name.split('/') {
            segments.push(segment);
        }
    }
    segments.push(&format!("{script_name}.{extension}"));
    drop(segments);
    Some(uri.into())
}

fn collect_inline_project_sources(
    scripts: &std::collections::BTreeMap<String, super::model::Script>,
    folder_name: Option<&str>,
    pending: &mut Vec<PendingProjectSource>,
    total_bytes: &mut usize,
) {
    for (script_name, script) in scripts {
        if pending.len() == MAX_PROJECT_SOURCE_FILES {
            return;
        }
        if let super::model::Script::Folder(_, children) = script {
            let child_folder = folder_name.map_or_else(
                || script_name.clone(),
                |folder| format!("{folder}/{script_name}"),
            );
            collect_inline_project_sources(children, Some(&child_folder), pending, total_bytes);
            continue;
        }
        let (kind, language, text) = match script {
            super::model::Script::Alias(definition) => (
                CodeDocument::Alias,
                script_language(definition.language),
                definition.script.as_deref(),
            ),
            super::model::Script::Trigger(definition) => (
                CodeDocument::Trigger,
                script_language(definition.language),
                definition.script.as_deref(),
            ),
            super::model::Script::Hotkey(definition) => (
                CodeDocument::Hotkey,
                script_language(definition.language),
                definition.script.as_deref(),
            ),
            super::model::Script::Folder(_, _) => unreachable!(),
        };
        let Some(text) = text.filter(|_| supports_language_service(language)) else {
            continue;
        };
        let remaining = MAX_PROJECT_SOURCE_TEXT_BYTES.saturating_sub(*total_bytes);
        if text.len() > MAX_DOCUMENT_BYTES.min(remaining) || validate_document_text(text).is_err() {
            continue;
        }
        let key = LanguageSourceKey::InlineAutomation {
            kind,
            language,
            folder_name: folder_name.map(str::to_owned),
            script_name: script_name.clone(),
        };
        let Some(uri) = language_source_key_uri(&key) else {
            continue;
        };
        *total_bytes += text.len();
        pending.push(PendingProjectSource {
            key,
            uri,
            language,
            kind: DocumentKind::Dependency,
            text: text.to_owned(),
        });
    }
}

fn valid_project_subpath(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains(['\\', '#', '?'])
        && !path.chars().any(char::is_control)
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

const fn supports_project_source(language: Language) -> bool {
    !matches!(language, Language::PlainText)
}

const fn language_extension(language: Language) -> &'static str {
    match language {
        Language::JavaScript => "js",
        Language::TypeScript => "ts",
        Language::JavaScriptReact => "jsx",
        Language::TypeScriptReact => "tsx",
        Language::Json => "json",
        Language::PlainText => "txt",
    }
}

fn allocate_document_id() -> DocumentId {
    loop {
        if let Some(id) = DocumentId::from_bytes(*smudgy_cloud::Uuid::new_v4().as_bytes()) {
            break id;
        }
    }
}

fn document_descriptor(
    document_id: DocumentId,
    uri: String,
    language: Language,
    kind: CodeDocument,
) -> DocumentDescriptor {
    let kind = match kind {
        CodeDocument::Alias => DocumentKind::InlineAutomation {
            automation_kind: AutomationKind::Alias,
        },
        CodeDocument::Trigger => DocumentKind::InlineAutomation {
            automation_kind: AutomationKind::Trigger,
        },
        CodeDocument::Hotkey => DocumentKind::InlineAutomation {
            automation_kind: AutomationKind::Hotkey,
        },
        CodeDocument::StandaloneModule => DocumentKind::StandaloneModule,
        CodeDocument::OwnedPackage => DocumentKind::OwnedPackage,
    };
    DocumentDescriptor {
        document: DocumentRef {
            key: DocumentKey {
                project: language_service_project(),
                document_id,
            },
            view: None,
            version: DocumentVersion::new(1).expect("one is a valid document version"),
        },
        uri,
        language,
        kind,
        analysis_context: AnalysisContextId::new(1)
            .expect("one is a valid language-service analysis context"),
        disk_revision: None,
    }
}

fn next_wire_value<T>(next: &mut u64) -> T
where
    T: TryFrom<u64>,
    T::Error: std::fmt::Debug,
{
    let current = (*next).max(1);
    *next = current.saturating_add(1);
    T::try_from(current).unwrap_or_else(|_| {
        *next = 2;
        T::try_from(1).expect("one is valid for every language-service wire counter")
    })
}

fn range_fits(range: Utf16Range, text: &str) -> bool {
    range.to_byte_range(text).is_ok()
}

fn scalar_to_utf16(position: ScalarPosition, text: &str) -> Option<Utf16Position> {
    let line = text.split('\n').nth(usize::try_from(position.line).ok()?)?;
    let scalar_column = usize::try_from(position.character).ok()?;
    let mut utf16_column = 0_usize;
    let mut scalars = line.chars();
    for _ in 0..scalar_column {
        utf16_column = utf16_column.checked_add(scalars.next()?.len_utf16())?;
    }
    Some(Utf16Position {
        line: position.line,
        character: u32::try_from(utf16_column).ok()?,
    })
}

fn utf16_to_scalar(position: Utf16Position, text: &str) -> Option<ScalarPosition> {
    let byte_offset = position.to_byte_offset(text).ok()?;
    let line_start = text[..byte_offset]
        .rfind('\n')
        .map_or(0, |offset| offset.saturating_add(1));
    Some(ScalarPosition {
        line: position.line,
        character: u32::try_from(text[line_start..byte_offset].chars().count()).ok()?,
    })
}

fn simultaneous_text_edits_fit(edits: &[TextEdit], text: &str) -> bool {
    let mut ranges = Vec::with_capacity(edits.len());
    for edit in edits {
        let Ok(range) = edit.range.to_byte_range(text) else {
            return false;
        };
        ranges.push(range);
    }
    ranges.sort_by_key(|range| (range.start, range.end));
    ranges.windows(2).all(|pair| {
        let left = &pair[0];
        let right = &pair[1];
        let duplicate_empty = left.is_empty() && right.is_empty() && left.start == right.start;
        left.end <= right.start && !duplicate_empty
    })
}

fn project_identity_matches_document(
    project: ProjectStateIdentity,
    document: DocumentStateIdentity,
) -> bool {
    project.project == document.document.key.project
        && project.graph_generation == document.graph_generation
        && project.service_generation == document.service_generation
        && project.worker_generation == document.worker_generation
}

fn diagnostic_ranges_fit(
    result: &DiagnosticsResult,
    current_document: DocumentId,
    text: &str,
) -> bool {
    result.items.iter().all(|diagnostic| {
        range_fits(diagnostic.range, text)
            && diagnostic.related_information.iter().all(|related| {
                related.document_id != current_document || range_fits(related.range, text)
            })
    })
}

fn completion_ranges_fit(result: &CompletionResult, text: &str) -> bool {
    result.items.iter().all(|item| {
        item.primary_edit
            .iter()
            .chain(&item.additional_edits)
            .all(|edit| range_fits(edit.range, text))
    })
}

fn definition_ranges_fit(
    result: &DefinitionResult,
    current_document: DocumentId,
    text: &str,
) -> bool {
    result.targets.iter().all(|target| {
        target.document_id != current_document
            || (range_fits(target.target_range, text)
                && range_fits(target.target_selection_range, text))
    })
}

fn definition_target_ranges_fit(target: &DefinitionTarget, text: &str) -> bool {
    range_fits(target.target_range, text) && range_fits(target.target_selection_range, text)
}

impl<S, C> Drop for AutomationCodeEditor<S, C>
where
    S: EditorSurface,
    C: LanguageServiceChannel,
{
    fn drop(&mut self) {
        self.close_service_document();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    use smudgy_script::language_service::{
        AnalysisContextId, AutomationKind, ClientId, CommandSequence, CompletionItem,
        CompletionItemId, CompletionKind, DiagnosticCode, DiagnosticSeverity, DiagnosticsResult,
        DocumentId, DocumentKey, DocumentKind, DocumentRef, DocumentResult, DocumentResultIdentity,
        EventEnvelope, FormattingResult, GraphGeneration, InsertTextFormat, Language,
        MAX_DOCUMENT_CHANGES, MarkupContent, MarkupKind, PROTOCOL_VERSION, ProjectId, ProjectScope,
        ProjectStateIdentity, ProjectStatusEvent, ServiceGeneration, SignatureHelpParameter,
        TextChange, Utf16Range, WorkerGeneration,
    };

    use super::*;

    #[test]
    fn module_path_languages_are_explicit_and_unknown_files_are_plaintext() {
        assert_eq!(path_language("main.js"), Language::JavaScript);
        assert_eq!(path_language("main.mjs"), Language::JavaScript);
        assert_eq!(path_language("main.ts"), Language::TypeScript);
        assert_eq!(path_language("main.mts"), Language::TypeScript);
        assert_eq!(path_language("view.tsx"), Language::TypeScriptReact);
        assert_eq!(path_language("data.json"), Language::Json);
        assert_eq!(path_language("notes.md"), Language::PlainText);
        assert_eq!(path_language("LICENSE"), Language::PlainText);
    }

    #[test]
    fn project_source_uris_preserve_safe_logical_paths_and_reject_ambiguous_ones() {
        assert_eq!(
            project_source_uri("modules", "combat/helpers.ts").as_deref(),
            Some("smudgy-project:///modules/combat/helpers.ts")
        );
        assert_eq!(
            owned_package_source_uri("my_package", "lib/view.tsx").as_deref(),
            Some("smudgy-project:///packages/my_package/lib/view.tsx")
        );
        assert!(project_source_uri("modules", "../escape.ts").is_none());
        assert!(project_source_uri("modules", "ambiguous#name.ts").is_none());
        assert!(project_source_uri("modules", "query?name.ts").is_none());
        assert!(project_source_uri("modules", "back\\slash.ts").is_none());
    }

    #[test]
    fn scalar_completion_positions_convert_to_utf16() {
        assert_eq!(
            scalar_to_utf16(
                ScalarPosition {
                    line: 0,
                    character: 2,
                },
                "🙂x"
            ),
            Some(Utf16Position {
                line: 0,
                character: 3,
            })
        );
        assert_eq!(
            scalar_to_utf16(
                ScalarPosition {
                    line: 1,
                    character: 1,
                },
                "first\n🙂x"
            ),
            Some(Utf16Position {
                line: 1,
                character: 2,
            })
        );
    }

    #[test]
    fn anchored_overlays_follow_scroll_and_stay_inside_the_editor() {
        let metrics = OverlayMetrics {
            viewport_width: 400.0,
            viewport_height: 240.0,
            viewport_scroll: 80.0,
            line_height: 18.0,
            char_width: 8.0,
        };
        let completion = completion_placement(SurfacePoint { x: 390.0, y: 290.0 }, metrics, 100.0);
        assert!(completion.x + completion.width <= metrics.viewport_width);
        assert!(completion.y + completion.height <= metrics.viewport_height);
        assert!(completion.y < 210.0, "insufficient room places it above");

        let hover = hover_placement(SurfacePoint { x: 395.0, y: 90.0 }, metrics, 80.0);
        assert!(hover.x + hover.width <= metrics.viewport_width);
        assert!(hover.y + hover.height <= metrics.viewport_height);
    }

    #[test]
    fn cross_file_definition_requires_complete_target_ranges_to_fit() {
        let mut target = DefinitionTarget {
            document_id: DocumentId::try_from([9_u8; 16]).expect("non-nil target ID"),
            target_range: Utf16Range {
                start: Utf16Position {
                    line: 0,
                    character: 0,
                },
                end: Utf16Position {
                    line: 0,
                    character: 8,
                },
            },
            target_selection_range: Utf16Range {
                start: Utf16Position {
                    line: 0,
                    character: 2,
                },
                end: Utf16Position {
                    line: 0,
                    character: 8,
                },
            },
            analyzed_uri: Some("smudgy-project:///modules/target.ts".to_owned()),
        };
        assert!(definition_target_ranges_fit(&target, "🙂target\n"));

        target.target_selection_range.end.character = 99;
        assert!(!definition_target_ranges_fit(&target, "🙂target\n"));
    }

    #[test]
    fn confirmed_same_package_navigation_reseeds_discarded_manifest_draft() {
        let manifest = smudgy_script::PackageManifest::parse(r#"{"version":"1.0.0"}"#)
            .expect("parse canonical manifest");
        let mut window = super::super::AutomationsWindow::new(
            iced::window::Id::unique(),
            "definition-manifest-discard-test".to_owned(),
            crate::cloud_account::test_handles(),
            smudgy_core::session::SessionId::from(1),
        );
        window.local_package = Some(Box::new(
            smudgy_core::models::local_packages::LocalPackage {
                name: "demo".to_owned(),
                manifest: manifest.clone(),
                readme: None,
                modules: Vec::new(),
            },
        ));
        let mut draft = super::super::manifest::ManifestDraft::from_manifest(&manifest);
        draft.version = "unsaved".to_owned();
        window.manifest_draft = Some(draft);
        window.manifest_dirty = true;
        window.manifest_editing = true;
        window.dirty = true;

        window.accept_discarded_navigation();

        assert!(!window.dirty);
        assert!(!window.manifest_dirty);
        assert!(!window.manifest_editing);
        assert_eq!(
            window
                .manifest_draft
                .as_ref()
                .expect("reseeded manifest")
                .version,
            "1.0.0"
        );
    }

    #[derive(Debug)]
    enum SurfaceMessage {
        Replace(String),
        Navigate,
        Passive,
        RequestCompletion(ScalarPosition),
        RequestSignatureHelp {
            position: ScalarPosition,
            starts_new_lifecycle: bool,
        },
        Hover(HoverUpdate),
    }

    #[derive(Debug)]
    struct FakeSurface {
        text: String,
        modified: bool,
        focused: Cell<bool>,
        goto: Cell<Option<ScalarPosition>>,
    }

    impl FakeSurface {
        fn new(text: &str) -> Self {
            Self {
                text: text.to_owned(),
                modified: false,
                focused: Cell::new(false),
                goto: Cell::new(None),
            }
        }
    }

    impl EditorSurface for FakeSurface {
        type Message = SurfaceMessage;
        type Effect = ();

        fn content(&self) -> String {
            self.text.clone()
        }

        fn update(&mut self, message: &Self::Message) -> SurfaceUpdate<Self::Effect> {
            match message {
                SurfaceMessage::Replace(text) => {
                    self.text.clone_from(text);
                    self.modified = true;
                    SurfaceUpdate {
                        effect: (),
                        changes: Some(DocumentChanges {
                            changes: vec![TextChange {
                                range: None,
                                text: text.clone(),
                            }],
                        }),
                        completion: None,
                        signature_help: None,
                        hover: HoverUpdate::Unchanged,
                        semantic_context_changed: true,
                        definition: None,
                        formatting: None,
                    }
                }
                SurfaceMessage::Navigate => SurfaceUpdate {
                    effect: (),
                    changes: None,
                    completion: None,
                    signature_help: None,
                    hover: HoverUpdate::Unchanged,
                    semantic_context_changed: true,
                    definition: None,
                    formatting: None,
                },
                SurfaceMessage::Passive => SurfaceUpdate {
                    effect: (),
                    changes: None,
                    completion: None,
                    signature_help: None,
                    hover: HoverUpdate::Unchanged,
                    semantic_context_changed: false,
                    definition: None,
                    formatting: None,
                },
                SurfaceMessage::RequestCompletion(position) => SurfaceUpdate {
                    effect: (),
                    changes: None,
                    completion: Some(CompletionIntent {
                        position: *position,
                        anchor: SurfacePoint { x: 24.0, y: 36.0 },
                    }),
                    signature_help: None,
                    hover: HoverUpdate::Unchanged,
                    semantic_context_changed: false,
                    definition: None,
                    formatting: None,
                },
                SurfaceMessage::RequestSignatureHelp {
                    position,
                    starts_new_lifecycle,
                } => SurfaceUpdate {
                    effect: (),
                    changes: None,
                    completion: None,
                    signature_help: Some(SignatureHelpIntent {
                        position: *position,
                        anchor: SurfacePoint { x: 24.0, y: 36.0 },
                        starts_new_lifecycle: *starts_new_lifecycle,
                    }),
                    hover: HoverUpdate::Unchanged,
                    semantic_context_changed: true,
                    definition: None,
                    formatting: None,
                },
                SurfaceMessage::Hover(hover) => SurfaceUpdate {
                    effect: (),
                    changes: None,
                    completion: None,
                    signature_help: None,
                    hover: *hover,
                    semantic_context_changed: false,
                    definition: None,
                    formatting: None,
                },
            }
        }

        fn apply_completion(
            &mut self,
            item: &smudgy_script::language_service::CompletionItem,
        ) -> SurfaceUpdate<Self::Effect> {
            let text = item.insert_text.as_deref().unwrap_or(&item.label);
            self.text.push_str(text);
            self.modified = true;
            SurfaceUpdate {
                effect: (),
                changes: Some(DocumentChanges {
                    changes: vec![TextChange {
                        range: None,
                        text: self.text.clone(),
                    }],
                }),
                completion: None,
                signature_help: Some(SignatureHelpIntent {
                    position: ScalarPosition {
                        line: 0,
                        character: u32::try_from(self.text.chars().count()).unwrap_or(u32::MAX),
                    },
                    anchor: SurfacePoint { x: 24.0, y: 36.0 },
                    starts_new_lifecycle: false,
                }),
                hover: HoverUpdate::Clear,
                semantic_context_changed: true,
                definition: None,
                formatting: None,
            }
        }

        fn apply_text_edits(&mut self, edits: &[TextEdit]) -> Result<Option<DocumentChanges>, ()> {
            let before = self.text.clone();
            let mut replacements = edits
                .iter()
                .map(|edit| {
                    edit.range
                        .to_byte_range(&before)
                        .map(|range| (range, edit.new_text.clone()))
                        .map_err(|_| ())
                })
                .collect::<Result<Vec<_>, ()>>()?;
            replacements.sort_by_key(|(range, _)| range.start);
            for (range, text) in replacements.into_iter().rev() {
                self.text.replace_range(range, &text);
            }
            if self.text == before {
                return Ok(None);
            }
            self.modified = true;
            Ok(Some(DocumentChanges {
                changes: vec![TextChange {
                    range: None,
                    text: self.text.clone(),
                }],
            }))
        }

        fn goto_position(&mut self, position: ScalarPosition) -> Self::Effect {
            self.goto.set(Some(position));
        }

        fn reset(&mut self, text: &str, _language: Language) -> Self::Effect {
            self.text = text.to_owned();
            self.modified = false;
        }

        fn is_modified(&self) -> bool {
            self.modified
        }

        fn mark_saved(&mut self) {
            self.modified = false;
        }

        fn request_focus(&self) {
            self.focused.set(true);
        }

        fn lose_focus(&mut self) {
            self.focused.set(false);
        }

        fn is_dialog_open(&self) -> bool {
            false
        }
    }

    #[derive(Clone)]
    struct FakeChannel {
        commands: Rc<RefCell<Vec<Command>>>,
        fail: Rc<Cell<bool>>,
    }

    type FakeChannelParts = (FakeChannel, Rc<RefCell<Vec<Command>>>, Rc<Cell<bool>>);

    impl FakeChannel {
        fn new() -> FakeChannelParts {
            let commands = Rc::new(RefCell::new(Vec::new()));
            let fail = Rc::new(Cell::new(false));
            (
                Self {
                    commands: Rc::clone(&commands),
                    fail: Rc::clone(&fail),
                },
                commands,
                fail,
            )
        }
    }

    impl LanguageServiceChannel for FakeChannel {
        type Error = ();

        fn send(&mut self, command: Command) -> Result<(), Self::Error> {
            if self.fail.get() {
                return Err(());
            }
            self.commands.borrow_mut().push(command);
            Ok(())
        }
    }

    fn number<T>(value: u64) -> T
    where
        T: TryFrom<u64>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value).expect("test identity must be valid")
    }

    fn document_id(value: u8) -> DocumentId {
        DocumentId::try_from([value; 16]).expect("test document identity must be valid")
    }

    fn descriptor(document_id_byte: u8, version: u64, uri: &str) -> DocumentDescriptor {
        DocumentDescriptor {
            document: DocumentRef {
                key: DocumentKey {
                    project: ProjectScope {
                        client_id: number::<ClientId>(1),
                        project_id: number::<ProjectId>(2),
                    },
                    document_id: document_id(document_id_byte),
                },
                view: None,
                version: number::<DocumentVersion>(version),
            },
            uri: uri.to_owned(),
            language: Language::TypeScript,
            kind: DocumentKind::InlineAutomation {
                automation_kind: AutomationKind::Alias,
            },
            analysis_context: number::<AnalysisContextId>(7),
            disk_revision: Some(number::<DiskRevision>(8)),
        }
    }

    fn state(document: DocumentRef) -> DocumentStateIdentity {
        state_with_worker(document, 13)
    }

    fn state_with_worker(document: DocumentRef, worker_generation: u64) -> DocumentStateIdentity {
        DocumentStateIdentity {
            document,
            graph_generation: number::<GraphGeneration>(11),
            service_generation: number::<ServiceGeneration>(12),
            worker_generation: number::<WorkerGeneration>(worker_generation),
        }
    }

    fn event(event: Event) -> EventEnvelope {
        EventEnvelope {
            protocol_version: PROTOCOL_VERSION,
            command_sequence: number::<CommandSequence>(1),
            event,
        }
    }

    fn completion_item(id: u64) -> CompletionItem {
        CompletionItem {
            id: CompletionItemId::new(id).unwrap(),
            label: format!("item {id}"),
            detail: None,
            documentation: None,
            kind: CompletionKind::Text,
            deprecated: false,
            filter_text: None,
            sort_text: None,
            insert_text: None,
            insert_text_format: InsertTextFormat::PlainText,
            primary_edit: None,
            additional_edits: Vec::new(),
        }
    }

    fn signature_help(position: Utf16Position) -> SignatureHelpResult {
        SignatureHelpResult {
            applicable_range: Utf16Range {
                start: position,
                end: position,
            },
            prefix: "send(".to_owned(),
            separator: ", ".to_owned(),
            suffix: "): void".to_owned(),
            parameters: vec![
                SignatureHelpParameter {
                    label: "target: string".to_owned(),
                    documentation: Some(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: "Where to send.".to_owned(),
                    }),
                    is_optional: false,
                    is_rest: false,
                },
                SignatureHelpParameter {
                    label: "text?: string".to_owned(),
                    documentation: None,
                    is_optional: true,
                    is_rest: false,
                },
            ],
            active_parameter: Some(0),
            selected_signature: 0,
            signature_count: 1,
            argument_count: 0,
            documentation: Some(MarkupContent {
                kind: MarkupKind::Markdown,
                value: "Sends text.".to_owned(),
            }),
        }
    }

    fn install_plain_hover(
        editor: &mut AutomationCodeEditor<FakeSurface, FakeChannel>,
        request_id: u64,
    ) -> DocumentResultIdentity {
        let identity = DocumentResultIdentity {
            state: state(editor.document.document),
            request_id: number::<RequestId>(request_id),
        };
        let position = Utf16Position::default();
        editor.hover_position = Some(position);
        editor.results.hover = Some(AcceptedHover::new(
            identity,
            Some(position),
            HoverResult {
                range: None,
                contents: MarkupContent {
                    kind: MarkupKind::PlainText,
                    value: "documentation".to_owned(),
                },
            },
            SurfacePoint { x: 12.0, y: 18.0 },
        ));
        identity
    }

    #[test]
    fn rich_hover_parses_markdown_and_highlights_fenced_typescript() {
        let descriptor = descriptor(3, 1, "smudgy-inline:///alias/test.ts");
        let identity = DocumentResultIdentity {
            state: state(descriptor.document),
            request_id: number::<RequestId>(90),
        };
        let accepted = AcceptedHover::new(
            identity,
            Some(Utf16Position::default()),
            HoverResult {
                range: None,
                contents: MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: concat!(
                        "Use `value` with **care** and ",
                        "[createEvent](#smudgy-jsdoc-link-1).\n\n",
                        "```ts\nconst value: number = 1;\n```"
                    )
                    .to_owned(),
                },
            },
            SurfacePoint::default(),
        );

        let HoverPresentation::Markdown(content) = &accepted.presentation else {
            panic!("Markdown hover must retain parsed Markdown content");
        };
        let viewer = smudgy_widgets::SmudgyMarkdownViewer::current();
        let style = rich_markdown_settings(&viewer).style;
        let paragraph_has_inline_code = content.items().iter().any(|item| {
            matches!(
                item,
                iced::widget::markdown::Item::Paragraph(text)
                    if text.spans(style).iter().any(|span| span.highlight.is_some())
            )
        });
        let paragraph_has_link = content.items().iter().any(|item| {
            matches!(
                item,
                iced::widget::markdown::Item::Paragraph(text)
                    if text.spans(style).iter().any(|span| span.link.is_some())
            )
        });
        let code_block_is_highlighted = content.items().iter().any(|item| {
            matches!(
                item,
                iced::widget::markdown::Item::CodeBlock {
                    language: Some(language),
                    lines,
                    ..
                } if language == "ts"
                    && lines.iter().flat_map(|line| line.spans(style).iter().cloned().collect::<Vec<_>>())
                        .any(|span| span.color.is_some())
            )
        });
        assert!(paragraph_has_inline_code);
        assert!(paragraph_has_link);
        assert!(code_block_is_highlighted);
    }

    #[test]
    fn rich_hover_render_settings_retain_live_palette_and_geist_code_fonts() {
        let viewer = smudgy_widgets::SmudgyMarkdownViewer::current();
        let colors = viewer.colors();
        assert_eq!(colors, *smudgy_theme::markdown::current());

        let settings = rich_markdown_settings(&viewer);
        assert_eq!(settings.style.link_color, colors.link);
        assert_eq!(settings.style.inline_code_color, colors.code_foreground);
        assert_eq!(
            settings.style.inline_code_highlight.background,
            iced::Background::Color(colors.code_background)
        );
        assert_eq!(
            settings.style.inline_code_font,
            crate::assets::fonts::GEIST_MONO_VF
        );
        assert_eq!(
            settings.style.code_block_font,
            crate::assets::fonts::GEIST_MONO_VF
        );

        let presentation =
            HoverPresentation::Markdown(Box::new(iced::widget::markdown::Content::parse(
                "Body [link](#inert).\n\n```ts\nlet x = 1;\n```",
            )));
        let _view = rich_markup_view(&presentation);
    }

    #[test]
    fn plaintext_hover_keeps_markdown_punctuation_literal() {
        let descriptor = descriptor(3, 1, "smudgy-inline:///alias/test.ts");
        let identity = DocumentResultIdentity {
            state: state(descriptor.document),
            request_id: number::<RequestId>(91),
        };
        let accepted = AcceptedHover::new(
            identity,
            None,
            HoverResult {
                range: None,
                contents: MarkupContent {
                    kind: MarkupKind::PlainText,
                    value: "**literal** `ticks`".to_owned(),
                },
            },
            SurfacePoint::default(),
        );

        assert!(matches!(
            accepted.presentation,
            HoverPresentation::PlainText(ref value) if value == "**literal** `ticks`"
        ));
    }

    #[test]
    fn hover_card_entry_cancels_grace_and_exit_expires_the_exact_result() {
        let (channel, _, _) = FakeChannel::new();
        let mut editor = AutomationCodeEditor::new(
            FakeSurface::new("value"),
            descriptor(3, 1, "smudgy-inline:///alias/test.ts"),
            Some(channel),
        );
        let identity = install_plain_hover(&mut editor, 92);
        let now = Instant::now();

        editor.leave_hover(now);
        assert!(editor.results.hover.is_some());
        assert!(editor.pending_hover_dismiss.is_some());
        assert!(editor.hover_overlay_entered(identity));
        assert_eq!(editor.hover_overlay_interactive, Some(identity));
        assert!(editor.pending_hover_dismiss.is_none());
        assert!(!editor.expire_hover_dismiss(now + HOVER_DISMISS_GRACE * 2));

        assert!(editor.hover_overlay_exited(identity, now));
        assert!(!editor.expire_hover_dismiss(now + HOVER_DISMISS_GRACE / 2));
        assert!(editor.results.hover.is_some());
        assert!(editor.expire_hover_dismiss(now + HOVER_DISMISS_GRACE));
        assert!(editor.results.hover.is_none());
    }

    #[test]
    fn hover_transit_keeps_docs_and_stale_card_messages_are_inert() {
        let (channel, _, _) = FakeChannel::new();
        let mut editor = AutomationCodeEditor::new(
            FakeSurface::new("value next"),
            descriptor(3, 1, "smudgy-inline:///alias/test.ts"),
            Some(channel),
        );
        let identity = install_plain_hover(&mut editor, 93);
        let stale_identity = DocumentResultIdentity {
            state: identity.state,
            request_id: number::<RequestId>(94),
        };
        let now = Instant::now();

        editor.leave_hover(now);
        editor.observe_hover_intent(
            HoverIntent {
                position: ScalarPosition {
                    line: 0,
                    character: 7,
                },
                anchor: SurfacePoint { x: 60.0, y: 18.0 },
            },
            now,
        );
        assert!(editor.results.hover.is_some());
        assert!(editor.pending_hover.is_some());
        assert!(!editor.hover_overlay_entered(stale_identity));
        assert!(!editor.hover_overlay_exited(stale_identity, now));

        assert!(editor.hover_overlay_entered(identity));
        assert!(editor.pending_hover.is_none());
        assert_eq!(editor.hover_position, Some(Utf16Position::default()));
        editor.update(&SurfaceMessage::Navigate);
        assert!(editor.results.hover.is_none());
    }

    #[test]
    fn leaving_during_a_replacement_hover_restores_the_accepted_position_for_retry() {
        let (channel, _, _) = FakeChannel::new();
        let mut editor = AutomationCodeEditor::new(
            FakeSurface::new("value next"),
            descriptor(3, 1, "smudgy-inline:///alias/test.ts"),
            Some(channel),
        );
        install_plain_hover(&mut editor, 95);
        let now = Instant::now();
        let replacement = HoverIntent {
            position: ScalarPosition {
                line: 0,
                character: 7,
            },
            anchor: SurfacePoint { x: 60.0, y: 18.0 },
        };

        editor.observe_hover_intent(replacement, now);
        assert_eq!(
            editor.hover_position,
            Some(Utf16Position {
                line: 0,
                character: 7,
            })
        );
        editor.leave_hover(now);
        assert_eq!(editor.hover_position, Some(Utf16Position::default()));

        editor.observe_hover_intent(replacement, now);
        assert!(editor.pending_hover.is_some());
        assert_eq!(editor.pending_hover.unwrap().position.character, 7);
    }

    #[test]
    fn failed_replacement_hover_restores_the_accepted_position_for_retry() {
        let (channel, _, _) = FakeChannel::new();
        let mut editor = AutomationCodeEditor::new(
            FakeSurface::new("value next"),
            descriptor(3, 1, "smudgy-inline:///alias/test.ts"),
            Some(channel),
        );
        let accepted = install_plain_hover(&mut editor, 96);
        editor.service_state = Some(accepted.state);
        editor.status = ServiceStatus::Ready;
        let now = Instant::now();
        let replacement = HoverIntent {
            position: ScalarPosition {
                line: 0,
                character: 7,
            },
            anchor: SurfacePoint { x: 60.0, y: 18.0 },
        };
        editor.observe_hover_intent(replacement, now);
        editor.pending_hover = None;
        let request_id = number::<RequestId>(97);
        editor.outstanding.set(RequestKind::Hover, Some(request_id));
        editor.hover_request_anchor = Some(replacement.anchor);

        assert_eq!(
            editor.apply_service_event(&event(Event::RequestFailed(
                smudgy_script::language_service::RequestFailure {
                    scope: FailureScope::Document(DocumentResultIdentity {
                        state: accepted.state,
                        request_id,
                    }),
                    code: "fixture".to_owned(),
                    retryable: true,
                    user_message: "retry".to_owned(),
                    log_detail: None,
                }
            ))),
            EventDisposition::Applied
        );
        assert_eq!(editor.hover_position, Some(Utf16Position::default()));
        assert!(editor.hover_request_anchor.is_none());

        editor.observe_hover_intent(replacement, now);
        assert!(editor.pending_hover.is_some());
    }

    #[test]
    fn hover_line_estimate_accounts_for_wrapped_jsdoc_paragraphs() {
        let source = "A long paragraph of JSDoc prose that should wrap across several lines in a narrow hover card instead of receiving the minimum two-line card.";
        let presentation =
            HoverPresentation::Markdown(Box::new(iced::widget::markdown::Content::parse(source)));

        assert!(presentation.estimated_lines(24) >= 6);
    }

    #[test]
    fn hover_line_estimate_does_not_wrap_horizontally_scrollable_code() {
        let source = "```ts\nfunction extraordinarilyLongSignature(firstParameter: string, secondParameter: number, thirdParameter: boolean): Promise<void>\n```";
        let presentation =
            HoverPresentation::Markdown(Box::new(iced::widget::markdown::Content::parse(source)));

        assert_eq!(presentation.estimated_lines(24), 2);
    }

    #[test]
    fn completion_helpers_cover_all_kinds_and_unicode_without_byte_splitting() {
        let kinds = [
            CompletionKind::Text,
            CompletionKind::Method,
            CompletionKind::Function,
            CompletionKind::Constructor,
            CompletionKind::Field,
            CompletionKind::Variable,
            CompletionKind::Class,
            CompletionKind::Interface,
            CompletionKind::TypeAlias,
            CompletionKind::Module,
            CompletionKind::Property,
            CompletionKind::Unit,
            CompletionKind::Value,
            CompletionKind::Enum,
            CompletionKind::Keyword,
            CompletionKind::Snippet,
            CompletionKind::Color,
            CompletionKind::File,
            CompletionKind::Reference,
            CompletionKind::Folder,
            CompletionKind::EnumMember,
            CompletionKind::Constant,
            CompletionKind::Struct,
            CompletionKind::Event,
            CompletionKind::Operator,
            CompletionKind::TypeParameter,
        ];
        assert!(
            kinds
                .into_iter()
                .all(|kind| !completion_kind_label(kind).is_empty())
        );
        assert_eq!(concise_completion_text("🙂🙂🙂", 2), "🙂🙂…");
        assert_eq!(reveal_completion_selection(0, 1, 500, 12), 0);
        assert_eq!(reveal_completion_selection(0, 11, 500, 12), 0);
        assert_eq!(reveal_completion_selection(0, 12, 500, 12), 1);
        assert_eq!(reveal_completion_selection(1, 499, 500, 12), 488);
        assert_eq!(reveal_completion_selection(488, 0, 500, 12), 0);
    }

    #[test]
    fn completion_overlay_height_is_compact_and_has_no_header_allowance() {
        assert_eq!(completion_desired_height(1), 34.0);
        assert_eq!(completion_desired_height(12), 320.0);
        assert_eq!(completion_desired_height(500), 320.0);
    }

    #[test]
    fn completion_row_washes_remain_distinct_on_light_and_dark_surfaces() {
        fn background(
            theme: &crate::theme::Theme,
            selected: bool,
            status: iced::widget::button::Status,
        ) -> iced::Color {
            match completion_row_style(selected)(theme, status).background {
                Some(iced::Background::Color(color)) => color,
                background => panic!("completion row needs a solid wash, got {background:?}"),
            }
        }

        let dark = crate::theme::Theme::default();
        let mut light = crate::theme::Theme::default();
        light.styles.general.background = iced::Color::from_rgb8(0xFD, 0xF6, 0xE3);
        light.styles.general.overlay_background = iced::Color::from_rgba8(0xFD, 0xF6, 0xE3, 0.92);
        light.styles.text.normal = iced::Color::from_rgb8(0x00, 0x2B, 0x36);

        for (name, theme) in [("dark", &dark), ("light", &light)] {
            let surface = completion_surface_color(theme);
            let normal = background(theme, false, iced::widget::button::Status::Active);
            let hovered = background(theme, false, iced::widget::button::Status::Hovered);
            let selected = background(theme, true, iced::widget::button::Status::Active);
            let selected_hovered = background(theme, true, iced::widget::button::Status::Hovered);

            assert!(
                contrast_ratio(normal, surface) >= 1.03,
                "{name} normal wash must remain visible"
            );
            assert!(
                contrast_ratio(hovered, surface) > contrast_ratio(normal, surface),
                "{name} hover must be stronger than the normal wash"
            );
            assert!(
                contrast_ratio(selected, surface) > contrast_ratio(hovered, surface),
                "{name} keyboard selection must be stronger than pointer hover"
            );
            assert!(
                contrast_ratio(selected_hovered, surface) > contrast_ratio(selected, surface),
                "{name} selected hover must remain distinct"
            );
        }
    }

    #[test]
    fn completion_kind_colors_group_symbols_semantically_and_dim_deprecated_items() {
        let theme = crate::theme::Theme::default();

        assert_eq!(
            completion_kind_color(&theme, CompletionKind::Function),
            completion_kind_color(&theme, CompletionKind::Method)
        );
        assert_eq!(
            completion_kind_color(&theme, CompletionKind::Class),
            completion_kind_color(&theme, CompletionKind::TypeAlias)
        );
        assert_eq!(
            completion_kind_color(&theme, CompletionKind::Variable),
            completion_kind_color(&theme, CompletionKind::Property)
        );
        assert_ne!(
            completion_kind_color(&theme, CompletionKind::Function),
            completion_kind_color(&theme, CompletionKind::Class)
        );
        assert_ne!(
            completion_kind_color(&theme, CompletionKind::Variable),
            completion_kind_color(&theme, CompletionKind::Keyword)
        );

        let regular = completion_kind_style(CompletionKind::Function, false)(&theme)
            .color
            .expect("completion kinds carry a visible color");
        let deprecated = completion_kind_style(CompletionKind::Function, true)(&theme)
            .color
            .expect("deprecated completion kinds retain their hue");
        assert_ne!(deprecated, regular);
        assert!(contrast_ratio(deprecated, completion_surface_color(&theme)) >= 2.25);

        let mut light = crate::theme::Theme::default();
        light.styles.general.background = iced::Color::from_rgb8(0xFD, 0xF6, 0xE3);
        light.styles.general.overlay_background = iced::Color::from_rgba8(0xFD, 0xF6, 0xE3, 0.92);
        light.styles.text.normal = iced::Color::from_rgb8(0x00, 0x2B, 0x36);
        for kind in [
            CompletionKind::Function,
            CompletionKind::Class,
            CompletionKind::Variable,
            CompletionKind::Module,
            CompletionKind::Keyword,
            CompletionKind::Event,
        ] {
            let color = completion_kind_color(&light, kind);
            assert!(contrast_ratio(color, completion_surface_color(&light)) >= 3.0);
        }
        let light_deprecated = completion_kind_style(CompletionKind::Function, true)(&light)
            .color
            .expect("light-palette deprecated kinds remain visible");
        assert!(contrast_ratio(light_deprecated, completion_surface_color(&light)) >= 2.25);
    }

    #[test]
    fn oversized_overlays_fit_one_side_of_the_source_line() {
        let (completion_height, completion_y) =
            fit_overlay_vertically(100.0, 18.0, 220.0, 500.0, true);
        assert!(completion_y >= 122.0);
        assert!(completion_y + completion_height <= 220.0);

        let (hover_height, hover_y) = fit_overlay_vertically(100.0, 18.0, 220.0, 500.0, false);
        assert!(hover_y + hover_height <= 100.0 || hover_y >= 118.0);
        assert!(hover_y + hover_height <= 220.0);
    }

    #[test]
    fn simultaneous_signature_and_completion_cards_never_overlap_or_cover_source() {
        let metrics = OverlayMetrics {
            viewport_width: 400.0,
            viewport_height: 220.0,
            viewport_scroll: 0.0,
            line_height: 18.0,
            char_width: 8.0,
        };

        for anchor_y in [0.0, 100.0, 202.0] {
            let anchor = SurfacePoint {
                x: 120.0,
                y: anchor_y,
            };
            let (signature, completion) =
                coordinated_signature_completion_placements(anchor, 180.0, anchor, 180.0, metrics);
            let source_top = anchor_y;
            let source_bottom = anchor_y + metrics.line_height;
            for placement in [signature, completion] {
                assert!(placement.height >= 20.0);
                assert!(placement.y >= 0.0);
                assert!(placement.y + placement.height <= metrics.viewport_height);
                assert!(
                    placement.y + placement.height <= source_top || placement.y >= source_bottom
                );
            }
            assert!(
                signature.y + signature.height <= completion.y
                    || completion.y + completion.height <= signature.y
            );
        }
    }

    #[test]
    fn offscreen_overlay_anchors_are_not_considered_visible() {
        let metrics = OverlayMetrics {
            viewport_width: 400.0,
            viewport_height: 100.0,
            viewport_scroll: 50.0,
            line_height: 18.0,
            char_width: 8.0,
        };
        assert!(!anchor_is_visible(
            SurfacePoint { x: 10.0, y: 20.0 },
            metrics
        ));
        assert!(anchor_is_visible(
            SurfacePoint { x: 10.0, y: 50.0 },
            metrics
        ));
        assert!(!anchor_is_visible(
            SurfacePoint { x: 10.0, y: 170.0 },
            metrics
        ));
    }

    #[test]
    fn offscreen_hover_does_not_suppress_a_visible_signature_overlay() {
        let metrics = OverlayMetrics {
            viewport_width: 400.0,
            viewport_height: 100.0,
            viewport_scroll: 50.0,
            line_height: 18.0,
            char_width: 8.0,
        };
        let current_state = state(descriptor(3, 1, "smudgy-inline:///alias/test.ts").document);
        let position = Utf16Position::default();
        let signature = AcceptedSignatureHelp::new(
            DocumentResultIdentity {
                state: current_state,
                request_id: number::<RequestId>(90),
            },
            position,
            signature_help(position),
            SurfacePoint { x: 80.0, y: 80.0 },
        );
        let mut hover = AcceptedHover::new(
            DocumentResultIdentity {
                state: current_state,
                request_id: number::<RequestId>(91),
            },
            Some(position),
            HoverResult {
                range: None,
                contents: MarkupContent {
                    kind: MarkupKind::PlainText,
                    value: "Offscreen documentation".to_owned(),
                },
            },
            SurfacePoint { x: 12.0, y: 20.0 },
        );

        assert!(anchor_is_visible(signature.anchor, metrics));
        assert!(!anchor_is_visible(hover.anchor, metrics));
        assert!(signature_overlay_should_render(
            &signature,
            Some(&hover),
            metrics
        ));

        hover.anchor.y = 80.0;
        assert!(!signature_overlay_should_render(
            &signature,
            Some(&hover),
            metrics
        ));
    }

    #[test]
    fn navigation_invalidates_pending_and_outstanding_completions() {
        let (channel, _, _) = FakeChannel::new();
        let mut editor = AutomationCodeEditor::new(
            FakeSurface::new("console"),
            descriptor(3, 1, "smudgy-inline:///alias/test.ts"),
            Some(channel),
        );
        editor.pending_completion = Some(PendingCompletion {
            position: Utf16Position {
                line: 0,
                character: 7,
            },
            anchor: SurfacePoint::default(),
        });
        editor
            .outstanding
            .set(RequestKind::Completion, Some(number::<RequestId>(9)));
        editor.results.completion = Some(AcceptedCompletion {
            identity: DocumentResultIdentity {
                state: state(editor.document.document),
                request_id: number::<RequestId>(9),
            },
            result: CompletionResult {
                is_incomplete: false,
                items: vec![completion_item(1)],
            },
            anchor: SurfacePoint::default(),
            selected: 0,
            first_visible: 0,
            scroll_id: iced::widget::Id::unique(),
        });

        editor.update(&SurfaceMessage::Navigate);

        assert!(editor.pending_completion.is_none());
        assert!(editor.outstanding.completion.is_none());
        assert!(editor.results.completion.is_none());
    }

    #[test]
    fn passive_editor_messages_preserve_pending_and_visible_completion() {
        let (channel, _, _) = FakeChannel::new();
        let mut editor = AutomationCodeEditor::new(
            FakeSurface::new("console"),
            descriptor(3, 1, "smudgy-inline:///alias/test.ts"),
            Some(channel),
        );
        let current_state = state(editor.document.document);
        assert_eq!(
            editor.apply_service_event(&event(Event::StateAcknowledged(
                AcknowledgedState::DocumentOpened(current_state),
            ))),
            EventDisposition::Applied
        );

        editor.update(&SurfaceMessage::RequestCompletion(ScalarPosition {
            line: 0,
            character: 7,
        }));
        editor.update(&SurfaceMessage::Passive);
        assert!(editor.pending_completion.is_some());

        let request_id = number::<RequestId>(41);
        assert!(editor.request_pending_completion(request_id));
        let completion = DocumentResult {
            identity: DocumentResultIdentity {
                state: current_state,
                request_id,
            },
            analyzed_uri: Some(editor.document.uri.clone()),
            result: CompletionResult {
                is_incomplete: false,
                items: vec![completion_item(1)],
            },
        };
        assert_eq!(
            editor.apply_service_event(&event(Event::Completion(completion))),
            EventDisposition::Applied
        );

        editor.update(&SurfaceMessage::Passive);

        let accepted = editor
            .results
            .completion
            .as_ref()
            .expect("passive messages keep the accepted result visible");
        assert_eq!(accepted.identity.request_id, request_id);
        assert_eq!(accepted.anchor, SurfacePoint { x: 24.0, y: 36.0 });
    }

    #[test]
    fn signature_help_uses_the_post_edit_utf16_cursor_and_coexists_with_completion() {
        let (channel, commands, _) = FakeChannel::new();
        let mut editor = AutomationCodeEditor::new(
            FakeSurface::new("🙂send("),
            descriptor(3, 1, "smudgy-inline:///alias/test.ts"),
            Some(channel),
        );
        let current_state = state(editor.document.document);
        assert_eq!(
            editor.apply_service_event(&event(Event::StateAcknowledged(
                AcknowledgedState::DocumentOpened(current_state),
            ))),
            EventDisposition::Applied
        );

        editor.update(&SurfaceMessage::RequestSignatureHelp {
            position: ScalarPosition {
                line: 0,
                character: 6,
            },
            starts_new_lifecycle: true,
        });
        assert_eq!(
            editor
                .pending_signature_help
                .map(|pending| pending.position),
            Some(Utf16Position {
                line: 0,
                character: 7,
            })
        );
        let signature_request = number::<RequestId>(42);
        assert!(editor.request_pending_signature_help(signature_request));
        assert!(matches!(
            commands.borrow().last(),
            Some(Command::RequestSignatureHelp(request))
                if request.position.character == 7
        ));
        let request_position = Utf16Position {
            line: 0,
            character: 7,
        };
        assert_eq!(
            editor.apply_service_event(&event(Event::SignatureHelp(DocumentResult {
                identity: DocumentResultIdentity {
                    state: current_state,
                    request_id: signature_request,
                },
                analyzed_uri: Some(editor.document.uri.clone()),
                result: Some(signature_help(request_position)),
            }))),
            EventDisposition::Applied
        );
        assert!(editor.results.signature_help.is_some());

        let completion_request = number::<RequestId>(43);
        assert!(editor.request_completion(completion_request, request_position));
        assert_eq!(
            editor.apply_service_event(&event(Event::Completion(DocumentResult {
                identity: DocumentResultIdentity {
                    state: current_state,
                    request_id: completion_request,
                },
                analyzed_uri: Some(editor.document.uri.clone()),
                result: CompletionResult {
                    is_incomplete: false,
                    items: vec![completion_item(1)],
                },
            }))),
            EventDisposition::Applied
        );
        assert!(editor.results.signature_help.is_some());
        assert!(editor.results.completion.is_some());

        editor.dismiss_pointer_overlays();
        assert!(editor.results.completion.is_none());
        assert!(editor.results.signature_help.is_some());
    }

    #[test]
    fn escape_suppresses_passive_signature_retrigger_until_a_new_call_lifecycle() {
        let (channel, _, _) = FakeChannel::new();
        let mut editor = AutomationCodeEditor::new(
            FakeSurface::new("send("),
            descriptor(3, 1, "smudgy-inline:///alias/test.ts"),
            Some(channel),
        );
        let current_state = state(editor.document.document);
        assert_eq!(
            editor.apply_service_event(&event(Event::StateAcknowledged(
                AcknowledgedState::DocumentOpened(current_state),
            ))),
            EventDisposition::Applied
        );
        let position = ScalarPosition {
            line: 0,
            character: 5,
        };
        editor.update(&SurfaceMessage::RequestSignatureHelp {
            position,
            starts_new_lifecycle: true,
        });
        let dismissed_request = number::<RequestId>(44);
        assert!(editor.request_pending_signature_help(dismissed_request));

        editor.dismiss_overlays();
        assert!(editor.signature_help_suppressed);
        assert!(editor.pending_signature_help.is_none());
        assert!(editor.outstanding.signature_help.is_none());
        assert_eq!(
            editor.apply_service_event(&event(Event::SignatureHelp(DocumentResult {
                identity: DocumentResultIdentity {
                    state: current_state,
                    request_id: dismissed_request,
                },
                analyzed_uri: Some(editor.document.uri.clone()),
                result: Some(signature_help(Utf16Position {
                    line: 0,
                    character: 5,
                })),
            }))),
            EventDisposition::Stale
        );

        editor.update(&SurfaceMessage::RequestSignatureHelp {
            position,
            starts_new_lifecycle: false,
        });
        assert!(editor.pending_signature_help.is_none());
        assert!(editor.signature_help_suppressed);

        editor.update(&SurfaceMessage::RequestSignatureHelp {
            position,
            starts_new_lifecycle: true,
        });
        assert!(editor.pending_signature_help.is_some());
        assert!(!editor.signature_help_suppressed);
        let accepted_request = number::<RequestId>(45);
        assert!(editor.request_pending_signature_help(accepted_request));
        assert_eq!(
            editor.apply_service_event(&event(Event::SignatureHelp(DocumentResult {
                identity: DocumentResultIdentity {
                    state: current_state,
                    request_id: accepted_request,
                },
                analyzed_uri: Some(editor.document.uri.clone()),
                result: Some(signature_help(Utf16Position {
                    line: 0,
                    character: 5,
                })),
            }))),
            EventDisposition::Applied
        );
        assert!(editor.results.signature_help.is_some());
        editor.dismiss_overlays();
        assert!(editor.results.signature_help.is_none());

        let completion_identity = DocumentResultIdentity {
            state: current_state,
            request_id: number::<RequestId>(46),
        };
        editor.results.completion = Some(AcceptedCompletion {
            identity: completion_identity,
            result: CompletionResult {
                is_incomplete: false,
                items: vec![completion_item(1)],
            },
            anchor: SurfacePoint::default(),
            selected: 0,
            first_visible: 0,
            scroll_id: iced::widget::Id::unique(),
        });
        assert!(editor.apply_completion(0, completion_identity).is_some());
        assert!(editor.signature_help_suppressed);
        assert!(editor.pending_signature_help.is_none());
    }

    #[test]
    fn signature_help_rejects_an_unrelated_in_bounds_applicable_range() {
        let (channel, _, _) = FakeChannel::new();
        let mut editor = AutomationCodeEditor::new(
            FakeSurface::new("send("),
            descriptor(3, 1, "smudgy-inline:///alias/test.ts"),
            Some(channel),
        );
        let current_state = state(editor.document.document);
        assert_eq!(
            editor.apply_service_event(&event(Event::StateAcknowledged(
                AcknowledgedState::DocumentOpened(current_state),
            ))),
            EventDisposition::Applied
        );
        editor.update(&SurfaceMessage::RequestSignatureHelp {
            position: ScalarPosition {
                line: 0,
                character: 5,
            },
            starts_new_lifecycle: true,
        });
        let request_id = number::<RequestId>(48);
        assert!(editor.request_pending_signature_help(request_id));
        let mut unrelated = signature_help(Utf16Position {
            line: 0,
            character: 5,
        });
        unrelated.applicable_range = Utf16Range {
            start: Utf16Position {
                line: 0,
                character: 0,
            },
            end: Utf16Position {
                line: 0,
                character: 1,
            },
        };

        assert_eq!(
            editor.apply_service_event(&event(Event::SignatureHelp(DocumentResult {
                identity: DocumentResultIdentity {
                    state: current_state,
                    request_id,
                },
                analyzed_uri: Some(editor.document.uri.clone()),
                result: Some(unrelated),
            }))),
            EventDisposition::Invalid
        );
        assert!(editor.outstanding.signature_help.is_none());
        assert!(editor.results.signature_help.is_none());
    }

    #[test]
    fn deliberate_hover_temporarily_coexists_with_accepted_signature_help() {
        let (channel, _, _) = FakeChannel::new();
        let mut editor = AutomationCodeEditor::new(
            FakeSurface::new("send(value"),
            descriptor(3, 1, "smudgy-inline:///alias/test.ts"),
            Some(channel),
        );
        let current_state = state(editor.document.document);
        editor.service_state = Some(current_state);
        editor.status = ServiceStatus::Ready;
        editor.results.signature_help = Some(AcceptedSignatureHelp::new(
            DocumentResultIdentity {
                state: current_state,
                request_id: number::<RequestId>(46),
            },
            Utf16Position {
                line: 0,
                character: 10,
            },
            signature_help(Utf16Position {
                line: 0,
                character: 10,
            }),
            SurfacePoint { x: 80.0, y: 18.0 },
        ));

        let now = Instant::now();
        editor.observe_hover_intent(
            HoverIntent {
                position: ScalarPosition {
                    line: 0,
                    character: 6,
                },
                anchor: SurfacePoint { x: 48.0, y: 18.0 },
            },
            now,
        );
        let hover_request = number::<RequestId>(47);
        assert!(editor.request_pending_hover(hover_request, now + HOVER_DEBOUNCE));
        assert_eq!(
            editor.apply_service_event(&event(Event::Hover(DocumentResult {
                identity: DocumentResultIdentity {
                    state: current_state,
                    request_id: hover_request,
                },
                analyzed_uri: Some(editor.document.uri.clone()),
                result: Some(HoverResult {
                    range: None,
                    contents: MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: "Hovered docs.".to_owned(),
                    },
                }),
            }))),
            EventDisposition::Applied
        );
        assert!(editor.results.hover.is_some());
        assert!(editor.results.signature_help.is_some());

        editor.clear_hover();
        assert!(editor.results.signature_help.is_some());
    }

    #[test]
    fn active_signature_hides_only_the_exact_missing_close_parenthesis_problem() {
        let (channel, _, _) = FakeChannel::new();
        let mut editor = AutomationCodeEditor::new(
            FakeSurface::new("send("),
            descriptor(3, 1, "smudgy-inline:///alias/test.ts"),
            Some(channel),
        );
        let current_state = state(editor.document.document);
        editor.service_state = Some(current_state);
        editor.status = ServiceStatus::Ready;
        let position = Utf16Position {
            line: 0,
            character: 5,
        };
        editor.results.signature_help = Some(AcceptedSignatureHelp::new(
            DocumentResultIdentity {
                state: current_state,
                request_id: number::<RequestId>(44),
            },
            position,
            signature_help(position),
            SurfacePoint::default(),
        ));
        let diagnostic = |code, message: &str, character| Diagnostic {
            range: Utf16Range {
                start: Utf16Position { line: 0, character },
                end: Utf16Position { line: 0, character },
            },
            severity: DiagnosticSeverity::Error,
            code,
            source: Some("typescript".to_owned()),
            message: message.to_owned(),
            related_information: Vec::new(),
        };
        let mut foreign_source = diagnostic(Some(DiagnosticCode::Number(1005)), "')' expected.", 5);
        foreign_source.source = Some("host".to_owned());
        editor.results.diagnostics = vec![
            diagnostic(Some(DiagnosticCode::Number(1005)), "')' expected.", 5),
            diagnostic(Some(DiagnosticCode::Number(1005)), "'}' expected.", 5),
            diagnostic(Some(DiagnosticCode::Number(1005)), "')' expected.", 4),
            diagnostic(
                Some(DiagnosticCode::String("1005".to_owned())),
                "')' expected.",
                5,
            ),
            foreign_source,
        ];

        let visible = editor.visible_diagnostics().collect::<Vec<_>>();
        assert_eq!(visible.len(), 4);
        assert!(
            visible
                .iter()
                .any(|item| item.source.as_deref() == Some("host"))
        );

        editor.clear_signature_help();
        assert_eq!(editor.visible_diagnostics().count(), 5);
    }

    #[test]
    fn completion_keyboard_selection_navigates_the_complete_result() {
        fn selection(update: Option<CompletionNavigationUpdate>) -> Option<(usize, usize)> {
            update.map(|update| (update.selected, update.count))
        }

        let (channel, _, _) = FakeChannel::new();
        let mut editor = AutomationCodeEditor::new(
            FakeSurface::new("console"),
            descriptor(3, 1, "smudgy-inline:///alias/test.ts"),
            Some(channel),
        );
        let current_state = state(editor.document.document);
        editor.service_state = Some(current_state);
        editor.results.completion = Some(AcceptedCompletion {
            identity: DocumentResultIdentity {
                state: current_state,
                request_id: number::<RequestId>(60),
            },
            result: CompletionResult {
                is_incomplete: false,
                items: (1..=10).map(completion_item).collect(),
            },
            anchor: SurfacePoint::default(),
            selected: 0,
            first_visible: 0,
            scroll_id: iced::widget::Id::unique(),
        });

        for expected in 1..10 {
            assert_eq!(
                selection(editor.navigate_completion(CompletionNavigation::Next, 5)),
                Some((expected, 10))
            );
        }
        assert_eq!(
            selection(editor.navigate_completion(CompletionNavigation::Next, 5)),
            Some((0, 10))
        );
        assert_eq!(
            selection(editor.navigate_completion(CompletionNavigation::Previous, 5)),
            Some((9, 10))
        );
        assert_eq!(
            selection(editor.navigate_completion(CompletionNavigation::First, 5)),
            Some((0, 10))
        );
        assert_eq!(
            selection(editor.navigate_completion(CompletionNavigation::PageDown, 5)),
            Some((4, 10))
        );
        assert_eq!(
            selection(editor.navigate_completion(CompletionNavigation::PageUp, 5)),
            Some((0, 10))
        );
        assert_eq!(
            selection(editor.navigate_completion(CompletionNavigation::Last, 5)),
            Some((9, 10))
        );
        assert_eq!(
            selection(editor.navigate_completion(CompletionNavigation::PageUp, 1)),
            Some((8, 10))
        );
    }

    #[test]
    fn completion_beyond_the_old_eight_row_boundary_applies_normally() {
        let (channel, _, _) = FakeChannel::new();
        let mut editor = AutomationCodeEditor::new(
            FakeSurface::new(""),
            descriptor(3, 1, "smudgy-inline:///alias/test.ts"),
            Some(channel),
        );
        let current_state = state(editor.document.document);
        editor.service_state = Some(current_state);
        editor.results.completion = Some(AcceptedCompletion {
            identity: DocumentResultIdentity {
                state: current_state,
                request_id: number::<RequestId>(61),
            },
            result: CompletionResult {
                is_incomplete: false,
                items: (1..=20).map(completion_item).collect(),
            },
            anchor: SurfacePoint::default(),
            selected: 0,
            first_visible: 0,
            scroll_id: iced::widget::Id::unique(),
        });

        assert_eq!(
            editor
                .navigate_completion(CompletionNavigation::Last, 12)
                .map(|update| (update.selected, update.count)),
            Some((19, 20))
        );
        assert!(editor.apply_selected_completion().is_some());
        assert_eq!(editor.content(), "item 20");
        assert_eq!(
            editor
                .pending_signature_help
                .map(|pending| pending.position),
            Some(Utf16Position {
                line: 0,
                character: 7,
            })
        );
    }

    #[test]
    fn delayed_completion_click_cannot_apply_the_same_index_from_a_newer_result() {
        let (channel, _, _) = FakeChannel::new();
        let mut editor = AutomationCodeEditor::new(
            FakeSurface::new(""),
            descriptor(3, 1, "smudgy-inline:///alias/test.ts"),
            Some(channel),
        );
        let current_state = state(editor.document.document);
        assert_eq!(
            editor.apply_service_event(&event(Event::StateAcknowledged(
                AcknowledgedState::DocumentOpened(current_state),
            ))),
            EventDisposition::Applied
        );

        let first_request = number::<RequestId>(70);
        assert!(editor.request_completion(first_request, Utf16Position::default()));
        assert_eq!(
            editor.apply_service_event(&event(Event::Completion(DocumentResult {
                identity: DocumentResultIdentity {
                    state: current_state,
                    request_id: first_request,
                },
                analyzed_uri: Some(editor.document.uri.clone()),
                result: CompletionResult {
                    is_incomplete: false,
                    items: vec![completion_item(1)],
                },
            }))),
            EventDisposition::Applied
        );
        let stale_identity = editor.results.completion.as_ref().unwrap().identity;

        editor.update(&SurfaceMessage::RequestCompletion(ScalarPosition {
            line: 0,
            character: 0,
        }));
        let second_request = number::<RequestId>(71);
        assert!(editor.request_pending_completion(second_request));
        assert_eq!(
            editor.apply_service_event(&event(Event::Completion(DocumentResult {
                identity: DocumentResultIdentity {
                    state: current_state,
                    request_id: second_request,
                },
                analyzed_uri: Some(editor.document.uri.clone()),
                result: CompletionResult {
                    is_incomplete: false,
                    items: vec![completion_item(2)],
                },
            }))),
            EventDisposition::Applied
        );

        assert!(editor.apply_completion(0, stale_identity).is_none());
        assert_eq!(editor.content(), "");
        let current_identity = editor.results.completion.as_ref().unwrap().identity;
        assert!(editor.apply_completion(0, current_identity).is_some());
        assert_eq!(editor.content(), "item 2");
    }

    #[test]
    fn empty_completion_result_does_not_suppress_the_next_hover() {
        let (channel, _, _) = FakeChannel::new();
        let mut editor = AutomationCodeEditor::new(
            FakeSurface::new("value"),
            descriptor(3, 1, "smudgy-inline:///alias/test.ts"),
            Some(channel),
        );
        let current_state = state(editor.document.document);
        assert_eq!(
            editor.apply_service_event(&event(Event::StateAcknowledged(
                AcknowledgedState::DocumentOpened(current_state),
            ))),
            EventDisposition::Applied
        );
        editor.update(&SurfaceMessage::RequestCompletion(ScalarPosition {
            line: 0,
            character: 5,
        }));
        let request_id = number::<RequestId>(61);
        assert!(editor.request_pending_completion(request_id));
        assert_eq!(
            editor.apply_service_event(&event(Event::Completion(DocumentResult {
                identity: DocumentResultIdentity {
                    state: current_state,
                    request_id,
                },
                analyzed_uri: Some(editor.document.uri.clone()),
                result: CompletionResult {
                    is_incomplete: false,
                    items: Vec::new(),
                },
            }))),
            EventDisposition::Applied
        );
        assert!(editor.results.completion.is_none());

        editor.update(&SurfaceMessage::Hover(HoverUpdate::At(HoverIntent {
            position: ScalarPosition {
                line: 0,
                character: 0,
            },
            anchor: SurfacePoint { x: 8.0, y: 8.0 },
        })));

        assert!(editor.pending_hover.is_some());
    }

    #[test]
    fn dismiss_overlays_invalidates_pending_and_accepted_transient_results() {
        let (channel, _, _) = FakeChannel::new();
        let mut editor = AutomationCodeEditor::new(
            FakeSurface::new("value"),
            descriptor(3, 1, "smudgy-inline:///alias/test.ts"),
            Some(channel),
        );
        let current_state = state(editor.document.document);
        editor.pending_completion = Some(PendingCompletion {
            position: Utf16Position::default(),
            anchor: SurfacePoint::default(),
        });
        editor.results.completion = Some(AcceptedCompletion {
            identity: DocumentResultIdentity {
                state: current_state,
                request_id: number::<RequestId>(62),
            },
            result: CompletionResult {
                is_incomplete: false,
                items: vec![completion_item(1)],
            },
            anchor: SurfacePoint::default(),
            selected: 0,
            first_visible: 0,
            scroll_id: iced::widget::Id::unique(),
        });
        editor.hover_position = Some(Utf16Position::default());
        let hover_identity = DocumentResultIdentity {
            state: current_state,
            request_id: number::<RequestId>(63),
        };
        editor.results.hover = Some(AcceptedHover::new(
            hover_identity,
            Some(Utf16Position::default()),
            HoverResult {
                range: None,
                contents: MarkupContent {
                    kind: MarkupKind::PlainText,
                    value: "docs".to_owned(),
                },
            },
            SurfacePoint::default(),
        ));

        editor.dismiss_overlays();

        assert!(editor.pending_completion.is_none());
        assert!(editor.results.completion.is_none());
        assert!(editor.hover_position.is_none());
        assert!(editor.results.hover.is_none());
        assert_eq!(editor.content(), "value");
    }

    #[test]
    fn hover_debounces_by_word_and_accepts_only_the_exact_request() {
        let (channel, _, _) = FakeChannel::new();
        let mut editor = AutomationCodeEditor::new(
            FakeSurface::new("🙂value"),
            descriptor(3, 1, "smudgy-inline:///alias/test.ts"),
            Some(channel),
        );
        let current_state = state(editor.document.document);
        assert_eq!(
            editor.apply_service_event(&event(Event::StateAcknowledged(
                AcknowledgedState::DocumentOpened(current_state),
            ))),
            EventDisposition::Applied
        );
        let intent = HoverUpdate::At(HoverIntent {
            position: ScalarPosition {
                line: 0,
                character: 1,
            },
            anchor: SurfacePoint { x: 30.0, y: 18.0 },
        });

        editor.update(&SurfaceMessage::Hover(intent));
        let ready_at = editor.pending_hover.as_ref().unwrap().ready_at;
        editor.update(&SurfaceMessage::Hover(intent));
        assert_eq!(editor.pending_hover.as_ref().unwrap().ready_at, ready_at);
        assert!(!editor.request_pending_hover(
            number::<RequestId>(50),
            ready_at.checked_sub(Duration::from_millis(1)).unwrap()
        ));

        let request_id = number::<RequestId>(51);
        assert!(editor.request_pending_hover(request_id, ready_at));
        let stale = DocumentResult {
            identity: DocumentResultIdentity {
                state: current_state,
                request_id: number::<RequestId>(52),
            },
            analyzed_uri: Some(editor.document.uri.clone()),
            result: Some(HoverResult {
                range: None,
                contents: MarkupContent {
                    kind: MarkupKind::PlainText,
                    value: "wrong".to_owned(),
                },
            }),
        };
        assert_eq!(
            editor.apply_service_event(&event(Event::Hover(stale))),
            EventDisposition::Stale
        );
        let accepted = DocumentResult {
            identity: DocumentResultIdentity {
                state: current_state,
                request_id,
            },
            analyzed_uri: Some(editor.document.uri.clone()),
            result: Some(HoverResult {
                range: None,
                contents: MarkupContent {
                    kind: MarkupKind::PlainText,
                    value: "value docs".to_owned(),
                },
            }),
        };
        assert_eq!(
            editor.apply_service_event(&event(Event::Hover(accepted))),
            EventDisposition::Applied
        );
        assert_eq!(
            editor.results.hover.as_ref().map(|hover| hover.anchor),
            Some(SurfacePoint { x: 30.0, y: 18.0 })
        );
        assert_eq!(
            editor.hover_position,
            Some(Utf16Position {
                line: 0,
                character: 2,
            })
        );

        editor.update(&SurfaceMessage::Hover(HoverUpdate::Clear));
        assert!(editor.results.hover.is_none());
        assert!(editor.hover_position.is_none());
    }

    #[test]
    fn invalid_matching_completion_and_hover_release_their_request_state() {
        let (channel, _, _) = FakeChannel::new();
        let mut editor = AutomationCodeEditor::new(
            FakeSurface::new("value"),
            descriptor(3, 1, "smudgy-inline:///alias/test.ts"),
            Some(channel),
        );
        let current_state = state(editor.document.document);
        assert_eq!(
            editor.apply_service_event(&event(Event::StateAcknowledged(
                AcknowledgedState::DocumentOpened(current_state),
            ))),
            EventDisposition::Applied
        );

        let completion_request = number::<RequestId>(80);
        assert!(editor.request_completion(completion_request, Utf16Position::default()));
        let mut item = completion_item(1);
        item.primary_edit = Some(TextEdit {
            range: Utf16Range {
                start: Utf16Position {
                    line: 99,
                    character: 0,
                },
                end: Utf16Position {
                    line: 99,
                    character: 1,
                },
            },
            new_text: "bad".to_owned(),
        });
        assert_eq!(
            editor.apply_service_event(&event(Event::Completion(DocumentResult {
                identity: DocumentResultIdentity {
                    state: current_state,
                    request_id: completion_request,
                },
                analyzed_uri: Some(editor.document.uri.clone()),
                result: CompletionResult {
                    is_incomplete: false,
                    items: vec![item],
                },
            }))),
            EventDisposition::Invalid
        );
        assert!(editor.outstanding.completion.is_none());
        assert!(editor.completion_request_anchor.is_none());

        editor.update(&SurfaceMessage::Hover(HoverUpdate::At(HoverIntent {
            position: ScalarPosition {
                line: 0,
                character: 0,
            },
            anchor: SurfacePoint { x: 8.0, y: 8.0 },
        })));
        let ready_at = editor.pending_hover.as_ref().unwrap().ready_at;
        let hover_request = number::<RequestId>(81);
        assert!(editor.request_pending_hover(hover_request, ready_at));
        assert_eq!(
            editor.apply_service_event(&event(Event::Hover(DocumentResult {
                identity: DocumentResultIdentity {
                    state: current_state,
                    request_id: hover_request,
                },
                analyzed_uri: Some(editor.document.uri.clone()),
                result: Some(HoverResult {
                    range: Some(Utf16Range {
                        start: Utf16Position {
                            line: 99,
                            character: 0,
                        },
                        end: Utf16Position {
                            line: 99,
                            character: 1,
                        },
                    }),
                    contents: MarkupContent {
                        kind: MarkupKind::PlainText,
                        value: "bad".to_owned(),
                    },
                }),
            }))),
            EventDisposition::Invalid
        );
        assert!(editor.outstanding.hover.is_none());
        assert!(editor.hover_request_anchor.is_none());
        assert!(editor.hover_position.is_none());
    }

    #[test]
    fn lifecycle_sends_open_change_save_and_one_close() {
        let (channel, commands, _) = FakeChannel::new();
        {
            let mut editor = AutomationCodeEditor::new(
                FakeSurface::new("let x = 1;"),
                descriptor(3, 1, "smudgy-inline:///alias/test.ts"),
                Some(channel),
            );
            editor.update(&SurfaceMessage::Replace("let x = 2;".to_owned()));
            editor.mark_saved(number::<DiskRevision>(9));
            editor.close();
            editor.close();
            assert!(!editor.is_modified());
        }

        let commands = commands.borrow();
        assert!(
            matches!(&commands[0], Command::OpenDocument(command) if command.text == "let x = 1;")
        );
        assert!(matches!(
            &commands[1],
            Command::ChangeDocument(command)
                if command.document.version.get() == 1 && command.new_version.get() == 2
        ));
        assert!(
            matches!(&commands[2], Command::SaveDocument(command) if command.text == "let x = 2;")
        );
        assert_eq!(
            commands
                .iter()
                .filter(|command| matches!(command, Command::CloseDocument(_)))
                .count(),
            1
        );
    }

    #[test]
    fn exact_result_identity_and_uri_are_required() {
        let (channel, _, _) = FakeChannel::new();
        let mut editor = AutomationCodeEditor::new(
            FakeSurface::new("const value = 1;"),
            descriptor(3, 1, "smudgy-inline:///alias/test.ts"),
            Some(channel),
        );
        let current_state = state(editor.document.document);
        assert_eq!(
            editor.apply_service_event(&event(Event::StateAcknowledged(
                AcknowledgedState::DocumentOpened(current_state),
            ))),
            EventDisposition::Applied
        );
        let request_id = number::<RequestId>(20);
        assert!(editor.request_diagnostics(request_id));

        let result = DocumentResult {
            identity: DocumentResultIdentity {
                state: current_state,
                request_id,
            },
            analyzed_uri: Some(editor.document.uri.clone()),
            result: DiagnosticsResult {
                items: vec![Diagnostic {
                    range: smudgy_script::language_service::Utf16Range::default(),
                    severity: DiagnosticSeverity::Warning,
                    code: None,
                    source: Some("typescript".to_owned()),
                    message: "test".to_owned(),
                    related_information: Vec::new(),
                }],
            },
        };
        assert_eq!(
            editor.apply_service_event(&event(Event::Diagnostics(result.clone()))),
            EventDisposition::Applied
        );
        assert_eq!(editor.results().diagnostics.len(), 1);

        let next_request = number::<RequestId>(21);
        assert!(editor.request_diagnostics(next_request));
        let wrong_uri = DocumentResult {
            identity: DocumentResultIdentity {
                state: current_state,
                request_id: next_request,
            },
            analyzed_uri: Some("smudgy-inline:///alias/other.ts".to_owned()),
            result: result.result,
        };
        assert_eq!(
            editor.apply_service_event(&event(Event::Diagnostics(wrong_uri))),
            EventDisposition::Stale
        );
    }

    #[test]
    fn formatting_requires_the_exact_state_and_advances_one_authoritative_version() {
        let (channel, commands, _) = FakeChannel::new();
        let mut editor = AutomationCodeEditor::new(
            FakeSurface::new("🙂x\nz"),
            descriptor(3, 1, "smudgy-inline:///alias/test.ts"),
            Some(channel),
        );
        let current_state = state(editor.document.document);
        assert_eq!(
            editor.apply_service_event(&event(Event::StateAcknowledged(
                AcknowledgedState::DocumentOpened(current_state),
            ))),
            EventDisposition::Applied
        );
        let request_id = number::<RequestId>(30);
        assert!(editor.request_formatting(
            request_id,
            FormattingOptions {
                tab_size: 2,
                insert_spaces: true,
            },
        ));
        let result = FormattingResult {
            edits: vec![
                TextEdit {
                    range: Utf16Range {
                        start: Utf16Position {
                            line: 0,
                            character: 2,
                        },
                        end: Utf16Position {
                            line: 0,
                            character: 3,
                        },
                    },
                    new_text: "long".to_owned(),
                },
                TextEdit {
                    range: Utf16Range {
                        start: Utf16Position {
                            line: 1,
                            character: 0,
                        },
                        end: Utf16Position {
                            line: 1,
                            character: 0,
                        },
                    },
                    new_text: "  ".to_owned(),
                },
            ],
        };
        let stale_state = DocumentStateIdentity {
            graph_generation: number::<GraphGeneration>(10),
            ..current_state
        };
        let stale = DocumentResult {
            identity: DocumentResultIdentity {
                state: stale_state,
                request_id,
            },
            analyzed_uri: Some(editor.document.uri.clone()),
            result: result.clone(),
        };
        assert_eq!(
            editor.apply_service_event(&event(Event::Formatting(stale))),
            EventDisposition::Stale
        );
        assert_eq!(editor.content(), "🙂x\nz");
        assert_eq!(editor.document.document.version.get(), 1);

        let current = DocumentResult {
            identity: DocumentResultIdentity {
                state: current_state,
                request_id,
            },
            analyzed_uri: Some(editor.document.uri.clone()),
            result,
        };
        assert_eq!(
            editor.apply_service_event(&event(Event::Formatting(current))),
            EventDisposition::Applied
        );
        assert_eq!(editor.content(), "🙂long\n  z");
        assert_eq!(editor.document.document.version.get(), 2);
        assert!(editor.take_service_edit_applied());
        assert!(!editor.take_service_edit_applied());
        assert!(matches!(
            commands.borrow().last(),
            Some(Command::ChangeDocument(change))
                if change.document == current_state.document
                    && change.new_version == editor.document.document.version
                    && change.changes.changes == vec![TextChange {
                        range: None,
                        text: "🙂long\n  z".to_owned(),
                    }]
        ));
    }

    #[test]
    fn formatting_ranges_must_fit_and_must_not_overlap() {
        let text = "🙂abc";
        let edit = |start, end| TextEdit {
            range: Utf16Range {
                start: Utf16Position {
                    line: 0,
                    character: start,
                },
                end: Utf16Position {
                    line: 0,
                    character: end,
                },
            },
            new_text: "x".to_owned(),
        };

        assert!(simultaneous_text_edits_fit(&[edit(2, 3), edit(3, 5)], text));
        assert!(!simultaneous_text_edits_fit(
            &[edit(2, 4), edit(3, 5)],
            text
        ));
        assert!(!simultaneous_text_edits_fit(&[edit(1, 2)], text));
        assert!(!simultaneous_text_edits_fit(&[edit(99, 99)], text));
        assert!(!simultaneous_text_edits_fit(
            &[edit(3, 3), edit(3, 3)],
            text
        ));
    }

    #[test]
    fn same_document_definition_keeps_its_origin_and_moves_to_a_scalar_column() {
        let (channel, _, _) = FakeChannel::new();
        let mut editor = AutomationCodeEditor::new(
            FakeSurface::new("🙂target"),
            descriptor(3, 1, "smudgy-inline:///alias/test.ts"),
            Some(channel),
        );
        let current_state = state(editor.document.document);
        assert_eq!(
            editor.apply_service_event(&event(Event::StateAcknowledged(
                AcknowledgedState::DocumentOpened(current_state),
            ))),
            EventDisposition::Applied
        );
        let request_id = number::<RequestId>(31);
        assert!(editor.request_definition(
            request_id,
            Utf16Position {
                line: 0,
                character: 3,
            },
        ));
        let target = DefinitionTarget {
            document_id: current_state.document.key.document_id,
            target_range: Utf16Range {
                start: Utf16Position {
                    line: 0,
                    character: 2,
                },
                end: Utf16Position {
                    line: 0,
                    character: 8,
                },
            },
            target_selection_range: Utf16Range {
                start: Utf16Position {
                    line: 0,
                    character: 2,
                },
                end: Utf16Position {
                    line: 0,
                    character: 8,
                },
            },
            analyzed_uri: Some(editor.document.uri.clone()),
        };
        let definition = DocumentResult {
            identity: DocumentResultIdentity {
                state: current_state,
                request_id,
            },
            analyzed_uri: Some(editor.document.uri.clone()),
            result: DefinitionResult {
                targets: vec![target],
            },
        };
        assert_eq!(
            editor.apply_service_event(&event(Event::Definition(definition))),
            EventDisposition::Applied
        );
        let accepted = editor.take_definition().expect("accepted definition");
        assert_eq!(accepted.origin, current_state);
        let selection = accepted.result.targets[0].target_selection_range.start;

        assert!(editor.goto_utf16_position(selection).is_some());
        assert_eq!(
            editor.surface.goto.get(),
            Some(ScalarPosition {
                line: 0,
                character: 1,
            })
        );
    }

    #[test]
    fn edit_invalidates_old_service_state_without_losing_text_on_channel_failure() {
        let (channel, commands, fail) = FakeChannel::new();
        let mut editor = AutomationCodeEditor::new(
            FakeSurface::new("old"),
            descriptor(3, 1, "smudgy-inline:///alias/test.ts"),
            Some(channel),
        );
        let old_state = state(editor.document.document);
        assert_eq!(
            editor.apply_service_event(&event(Event::StateAcknowledged(
                AcknowledgedState::DocumentOpened(old_state),
            ))),
            EventDisposition::Applied
        );
        fail.set(true);
        editor.update(&SurfaceMessage::Replace("unsaved edit".to_owned()));

        assert_eq!(editor.content(), "unsaved edit");
        assert!(editor.is_modified());
        assert_eq!(editor.document.document.version.get(), 2);
        assert_eq!(editor.service_status(), ServiceStatus::Unavailable);
        assert_eq!(
            editor.apply_service_event(&event(Event::StateAcknowledged(
                AcknowledgedState::DocumentChanged(old_state),
            ))),
            EventDisposition::Stale
        );

        fail.set(false);
        editor.retry_service_sync();
        let commands = commands.borrow();
        assert!(matches!(
            commands.get(1),
            Some(Command::CloseDocument(command)) if command.document == old_state.document
        ));
        assert!(matches!(
            commands.get(2),
            Some(Command::OpenDocument(command))
                if command.descriptor.document == editor.document.document
                    && command.text == "unsaved edit"
        ));
    }

    #[test]
    fn rebind_closes_old_identity_before_opening_new_identity() {
        let (channel, commands, _) = FakeChannel::new();
        let mut editor = AutomationCodeEditor::new(
            FakeSurface::new("old"),
            descriptor(3, 1, "smudgy-inline:///alias/old.ts"),
            Some(channel),
        );
        editor.rebind(descriptor(4, 1, "smudgy-inline:///alias/new.ts"), "new");

        let commands = commands.borrow();
        assert!(matches!(
            &commands[1],
            Command::CloseDocument(command) if command.document.key.document_id == document_id(3)
        ));
        assert!(matches!(
            &commands[2],
            Command::OpenDocument(command)
                if command.descriptor.document.key.document_id == document_id(4)
        ));
        assert_eq!(editor.content(), "new");
        assert!(!editor.is_modified());
    }

    #[test]
    fn restart_replays_current_snapshot_and_fences_old_worker_events() {
        let (channel, commands, _) = FakeChannel::new();
        let mut editor = AutomationCodeEditor::new(
            FakeSurface::new("unsaved"),
            descriptor(3, 1, "smudgy-inline:///alias/test.ts"),
            Some(channel),
        );
        let old_state = state(editor.document.document);
        assert_eq!(
            editor.apply_service_event(&event(Event::StateAcknowledged(
                AcknowledgedState::DocumentOpened(old_state),
            ))),
            EventDisposition::Applied
        );

        assert_eq!(
            editor.apply_service_event(&event(Event::WorkerRestarted {
                worker_generation: number::<WorkerGeneration>(14),
            })),
            EventDisposition::Applied
        );
        assert!(matches!(
            commands.borrow().last(),
            Some(Command::OpenDocument(command)) if command.text == "unsaved"
        ));
        assert_eq!(
            editor.apply_service_event(&event(Event::StateAcknowledged(
                AcknowledgedState::DocumentOpened(old_state),
            ))),
            EventDisposition::Stale
        );
        let current_state = state_with_worker(editor.document.document, 14);
        assert_eq!(
            editor.apply_service_event(&event(Event::StateAcknowledged(
                AcknowledgedState::DocumentOpened(current_state),
            ))),
            EventDisposition::Applied
        );
    }

    #[test]
    fn degraded_status_is_not_overridden_by_a_late_document_ack() {
        let (channel, _, _) = FakeChannel::new();
        let mut editor = AutomationCodeEditor::new(
            FakeSurface::new("text"),
            descriptor(3, 1, "smudgy-inline:///alias/test.ts"),
            Some(channel),
        );
        let current_state = state(editor.document.document);
        let project = ProjectStateIdentity {
            project: current_state.document.key.project,
            graph_generation: current_state.graph_generation,
            service_generation: current_state.service_generation,
            worker_generation: current_state.worker_generation,
        };
        let hover_identity = install_plain_hover(&mut editor, 98);
        assert!(editor.hover_overlay_entered(hover_identity));
        editor.apply_service_event(&event(Event::ProjectStatus(ProjectStatusEvent {
            identity: project,
            status: ProjectStatus::Degraded {
                code: "fixture".to_owned(),
                message: "fixture".to_owned(),
            },
        })));
        editor.apply_service_event(&event(Event::StateAcknowledged(
            AcknowledgedState::DocumentOpened(current_state),
        )));
        editor.apply_service_event(&event(Event::StateAcknowledged(
            AcknowledgedState::DocumentSaved(current_state),
        )));

        assert_eq!(editor.service_status(), ServiceStatus::Unavailable);
        assert!(editor.results.hover.is_none());
        assert!(editor.hover_overlay_interactive.is_none());
        assert!(editor.pending_hover_dismiss.is_none());
        editor.observe_hover_intent(
            HoverIntent {
                position: ScalarPosition {
                    line: 0,
                    character: 1,
                },
                anchor: SurfacePoint { x: 16.0, y: 18.0 },
            },
            Instant::now(),
        );
        assert!(editor.pending_hover.is_some());
        assert!(!editor.request_diagnostics(number::<RequestId>(99)));
    }

    #[test]
    fn document_acknowledgements_advance_the_cached_project_generation() {
        let (channel, _, _) = FakeChannel::new();
        let mut editor = AutomationCodeEditor::new(
            FakeSurface::new("let value = 1;"),
            descriptor(3, 1, "smudgy-inline:///alias/test.ts"),
            Some(channel),
        );
        let project = editor.document.document.key.project;
        let worker_generation = number::<WorkerGeneration>(13);
        assert_eq!(
            editor.apply_service_event(&event(Event::ProjectStatus(ProjectStatusEvent {
                identity: ProjectStateIdentity {
                    project,
                    graph_generation: number::<GraphGeneration>(11),
                    service_generation: number::<ServiceGeneration>(12),
                    worker_generation,
                },
                status: ProjectStatus::Ready,
            }))),
            EventDisposition::Applied
        );

        let opened = DocumentStateIdentity {
            document: editor.document.document,
            graph_generation: number::<GraphGeneration>(11),
            service_generation: number::<ServiceGeneration>(13),
            worker_generation,
        };
        assert_eq!(
            editor.apply_service_event(&event(Event::StateAcknowledged(
                AcknowledgedState::DocumentOpened(opened),
            ))),
            EventDisposition::Applied
        );
        assert_eq!(editor.service_status(), ServiceStatus::Ready);

        editor.update(&SurfaceMessage::Replace("let value = 2;".to_owned()));
        let changed = DocumentStateIdentity {
            document: editor.document.document,
            service_generation: number::<ServiceGeneration>(14),
            ..opened
        };
        assert_eq!(
            editor.apply_service_event(&event(Event::StateAcknowledged(
                AcknowledgedState::DocumentChanged(changed),
            ))),
            EventDisposition::Applied
        );
        assert!(editor.request_diagnostics(number::<RequestId>(99)));
    }

    #[test]
    fn project_refresh_advances_the_open_overlay_fence() {
        let (channel, commands, _) = FakeChannel::new();
        let mut editor = AutomationCodeEditor::new(
            FakeSurface::new("let value = 1;"),
            descriptor(3, 1, "smudgy-project:///modules/main.ts"),
            Some(channel),
        );
        let opened = state(editor.document.document);
        assert_eq!(
            editor.apply_service_event(&event(Event::StateAcknowledged(
                AcknowledgedState::DocumentOpened(opened),
            ))),
            EventDisposition::Applied
        );
        let refreshed = ProjectStateIdentity {
            project: opened.document.key.project,
            graph_generation: number::<GraphGeneration>(12),
            service_generation: number::<ServiceGeneration>(13),
            worker_generation: opened.worker_generation,
        };
        editor.status = ServiceStatus::Unavailable;
        assert_eq!(
            editor.apply_service_event(&event(Event::StateAcknowledged(
                AcknowledgedState::ProjectRefreshed(refreshed),
            ))),
            EventDisposition::Applied
        );
        assert_eq!(editor.service_status(), ServiceStatus::Ready);
        assert!(editor.request_diagnostics(number::<RequestId>(55)));
        assert!(matches!(
            commands.borrow().last(),
            Some(Command::RequestDiagnostics(request))
                if request.identity.state.graph_generation == refreshed.graph_generation
                    && request.identity.state.service_generation == refreshed.service_generation
        ));
    }

    #[test]
    fn invalid_delta_resyncs_with_a_full_current_snapshot() {
        let (channel, commands, _) = FakeChannel::new();
        let mut editor = AutomationCodeEditor::new(
            FakeSurface::new("old"),
            descriptor(3, 1, "smudgy-inline:///alias/test.ts"),
            Some(channel),
        );
        editor.surface.text = "current".to_owned();
        editor.document_changed(DocumentChanges {
            changes: vec![
                TextChange {
                    range: None,
                    text: "current".to_owned(),
                };
                MAX_DOCUMENT_CHANGES + 1
            ],
        });

        let commands = commands.borrow();
        assert!(matches!(commands.get(1), Some(Command::CloseDocument(_))));
        assert!(matches!(
            commands.get(2),
            Some(Command::OpenDocument(command))
                if command.text == "current" && command.descriptor.document.version.get() == 2
        ));
    }

    #[test]
    fn out_of_bounds_results_and_post_close_events_are_rejected() {
        let (channel, commands, _) = FakeChannel::new();
        let mut editor = AutomationCodeEditor::new(
            FakeSurface::new("text"),
            descriptor(3, 1, "smudgy-inline:///alias/test.ts"),
            Some(channel),
        );
        let current_state = state(editor.document.document);
        editor.apply_service_event(&event(Event::StateAcknowledged(
            AcknowledgedState::DocumentOpened(current_state),
        )));
        let request_id = number::<RequestId>(20);
        assert!(editor.request_diagnostics(request_id));
        let invalid = DocumentResult {
            identity: DocumentResultIdentity {
                state: current_state,
                request_id,
            },
            analyzed_uri: Some(editor.document.uri.clone()),
            result: DiagnosticsResult {
                items: vec![Diagnostic {
                    range: Utf16Range {
                        start: Utf16Position {
                            line: 99,
                            character: 0,
                        },
                        end: Utf16Position {
                            line: 99,
                            character: 1,
                        },
                    },
                    severity: DiagnosticSeverity::Warning,
                    code: None,
                    source: None,
                    message: "invalid".to_owned(),
                    related_information: Vec::new(),
                }],
            },
        };
        assert_eq!(
            editor.apply_service_event(&event(Event::Diagnostics(invalid))),
            EventDisposition::Invalid
        );

        editor.close();
        let command_count = commands.borrow().len();
        editor.update(&SurfaceMessage::Replace("after close".to_owned()));
        editor.mark_saved(number::<DiskRevision>(100));
        assert_eq!(
            editor.apply_service_event(&event(Event::StateAcknowledged(
                AcknowledgedState::DocumentOpened(current_state),
            ))),
            EventDisposition::Stale
        );
        assert_eq!(commands.borrow().len(), command_count);
    }

    #[test]
    fn inline_dynamic_import_uses_ambient_types_without_attaching_module_sources() {
        let mut window = super::super::AutomationsWindow::new(
            iced::window::Id::unique(),
            "inline-dynamic-import-test".to_owned(),
            crate::cloud_account::test_handles(),
            smudgy_core::session::SessionId::from(1),
        );
        window.modules = vec![smudgy_core::models::modules::ModuleFile {
            subpath: "must-not-join-inline.ts".to_owned(),
            path: std::path::PathBuf::from("must-not-join-inline.ts"),
        }];
        let sources = window
            .language_project_sources(&LanguageProjectContext::Inline)
            .sources;
        assert_eq!(sources.len(), 1);
        assert_eq!(
            sources[0].uri, "smudgy-project:///inline/context.d.ts",
            "classic inline bodies must not acquire a false relative module graph"
        );
        assert_eq!(
            sources[0].text,
            smudgy_core::models::script_typings::language_service_inline_bridge(),
            "the inline-only declarations must be installed directly in the inline project"
        );
        assert!(
            !sources[0].text.contains("<reference path"),
            "the inline project must not bypass its scope through a global declaration path"
        );

        let _ = window.bind_code_editor(
            "void import(\"smudgy:core\").then((core) => core.createAlias);\n",
            Language::TypeScript,
            CodeDocument::Alias,
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let _ = window.poll_language_service();
            let analyzed = window.code_editor.as_ref().is_some_and(|editor| {
                editor.service_state.is_some()
                    && editor.outstanding.diagnostics.is_none()
                    && editor.service_status() == ServiceStatus::Ready
            });
            if analyzed {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "inline dynamic-import diagnostics timed out"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !window
                .code_editor
                .as_ref()
                .unwrap()
                .results()
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == Some(DiagnosticCode::Number(2307))),
            "managed ambient declarations must type supported absolute dynamic imports"
        );
    }

    #[test]
    fn oversized_saved_module_is_rejected_before_graph_read() {
        let directory = tempfile::tempdir().expect("temporary module directory");
        let path = directory.path().join("oversized.ts");
        let file = std::fs::File::create(&path).expect("create sparse oversized module");
        file.set_len(u64::try_from(MAX_DOCUMENT_BYTES + 1).expect("wire size fits u64"))
            .expect("size sparse oversized module");

        let mut window = super::super::AutomationsWindow::new(
            iced::window::Id::unique(),
            "oversized-language-graph-test".to_owned(),
            crate::cloud_account::test_handles(),
            smudgy_core::session::SessionId::from(1),
        );
        window.modules = vec![smudgy_core::models::modules::ModuleFile {
            subpath: "oversized.ts".to_owned(),
            path,
        }];

        assert!(
            window
                .language_project_sources(&LanguageProjectContext::Modules)
                .sources
                .is_empty(),
            "per-file graph cap must be applied from metadata before reading the file"
        );
    }

    #[test]
    fn cross_file_definition_is_mount_and_graph_fenced_and_uses_the_dirty_guard() {
        let directory = tempfile::tempdir().expect("temporary module directory");
        let origin_path = directory.path().join("origin.ts");
        let target_path = directory.path().join("target.ts");
        std::fs::write(&origin_path, "target();\n").expect("seed origin module");
        std::fs::write(&target_path, "🙂export const target = 1;\n").expect("seed target module");

        let mut window = super::super::AutomationsWindow::new(
            iced::window::Id::unique(),
            "definition-navigation-test".to_owned(),
            crate::cloud_account::test_handles(),
            smudgy_core::session::SessionId::from(1),
        );
        window.modules = vec![
            smudgy_core::models::modules::ModuleFile {
                subpath: "origin.ts".to_owned(),
                path: origin_path.clone(),
            },
            smudgy_core::models::modules::ModuleFile {
                subpath: "target.ts".to_owned(),
                path: target_path,
            },
        ];
        window.selection = super::super::Selection::Module("origin.ts".to_owned());
        window.pane = super::super::Pane::Module(super::super::ModuleState {
            mode: super::super::ModuleMode::View,
            subpath: "origin.ts".to_owned(),
            path: Some(origin_path),
            name: String::new(),
            tab: super::super::ModuleTab::Source,
            activation: smudgy_core::models::profile_activation::ProfileActivation::All,
            activation_touched: false,
            error: None,
        });
        window.language_project_context = Some(LanguageProjectContext::Modules);
        window.language_project_target_context = Some(LanguageProjectContext::Modules);
        let _ = window.bind_code_editor(
            "target();\n",
            Language::TypeScript,
            CodeDocument::StandaloneModule,
        );
        let target_id =
            window.language_source_id(LanguageSourceKey::Module("target.ts".to_owned()));
        let origin_document = window
            .code_editor
            .as_ref()
            .expect("origin editor")
            .document()
            .document;
        let origin = state(origin_document);
        let editor = window.code_editor.as_mut().expect("origin editor");
        editor.service_state = Some(origin);
        editor.worker_generation = Some(origin.worker_generation);
        editor.project_state = Some(ProjectStateIdentity {
            project: origin.document.key.project,
            graph_generation: origin.graph_generation,
            service_generation: origin.service_generation,
            worker_generation: origin.worker_generation,
        });
        editor.status = ServiceStatus::Ready;
        let navigation = DefinitionNavigation {
            origin,
            origin_mount_generation: window.code_editor_mount_generation,
            target: DefinitionTarget {
                document_id: target_id,
                target_range: Utf16Range {
                    start: Utf16Position {
                        line: 0,
                        character: 2,
                    },
                    end: Utf16Position {
                        line: 0,
                        character: 8,
                    },
                },
                target_selection_range: Utf16Range {
                    start: Utf16Position {
                        line: 0,
                        character: 2,
                    },
                    end: Utf16Position {
                        line: 0,
                        character: 8,
                    },
                },
                analyzed_uri: Some("smudgy-project:///modules/target.ts".to_owned()),
            },
        };

        let mut stale_mount = navigation.clone();
        stale_mount.origin_mount_generation = stale_mount.origin_mount_generation.saturating_add(1);
        let _ = window.navigate_code_definition(stale_mount);
        assert!(matches!(
            window.selection,
            super::super::Selection::Module(ref subpath) if subpath == "origin.ts"
        ));

        let mut stale_graph = navigation.clone();
        stale_graph.origin.graph_generation = number::<GraphGeneration>(10);
        let _ = window.navigate_code_definition(stale_graph);
        assert!(matches!(
            window.selection,
            super::super::Selection::Module(ref subpath) if subpath == "origin.ts"
        ));

        window.dirty = true;
        let _ = window.update(super::super::Message::NavigateCodeDefinition(
            navigation.clone(),
        ));
        assert!(matches!(
            window.pending_nav.as_deref(),
            Some(super::super::Message::NavigateCodeDefinition(_))
        ));
        assert_eq!(window.code_editor_text(), "target();\n");

        window
            .code_editor
            .as_mut()
            .expect("origin editor")
            .service_state
            .as_mut()
            .expect("origin service state")
            .graph_generation = number::<GraphGeneration>(10);
        let revision = window.pending_nav_revision;
        let _ = window.update(super::super::Message::ConfirmDiscardNavRevision(revision));
        assert!(
            window.dirty,
            "a stale confirmed jump must retain the dirty guard"
        );
        assert!(matches!(
            window.selection,
            super::super::Selection::Module(ref subpath) if subpath == "origin.ts"
        ));
        assert_eq!(window.code_editor_text(), "target();\n");

        window
            .code_editor
            .as_mut()
            .expect("origin editor")
            .service_state = Some(navigation.origin);
        let _ = window.update(super::super::Message::NavigateCodeDefinition(navigation));
        let revision = window.pending_nav_revision;
        let _ = window.update(super::super::Message::ConfirmDiscardNavRevision(revision));
        assert!(!window.dirty);
        assert!(matches!(
            window.selection,
            super::super::Selection::Module(ref subpath) if subpath == "target.ts"
        ));
        assert_eq!(window.code_editor_text(), "🙂export const target = 1;\n");
        assert_eq!(
            window
                .code_editor
                .as_ref()
                .expect("target editor")
                .surface
                .cursor_position(),
            (0, 1),
            "the UTF-16 target after the emoji must become the editor's scalar column"
        );
    }

    #[test]
    fn project_context_commits_only_on_its_exact_refresh_ack_and_retries_failure() {
        let mut window = super::super::AutomationsWindow::new(
            iced::window::Id::unique(),
            "project-refresh-fence-test".to_owned(),
            crate::cloud_account::test_handles(),
            smudgy_core::session::SessionId::from(1),
        );
        let previous = LanguageProjectContext::Modules;
        let next = LanguageProjectContext::OwnedPackage("next".to_owned());
        let graph_generation = number::<GraphGeneration>(20);
        let command_sequence = number::<CommandSequence>(21);
        let worker_generation = number::<WorkerGeneration>(22);
        window.language_project_context = Some(previous.clone());
        window.language_project_target_context = Some(next.clone());
        window.pending_language_project_refresh = Some(PendingLanguageProjectRefresh {
            context: next.clone(),
            graph_generation,
            command_sequence,
            retries_remaining: 1,
            inline_bridge: None,
        });

        let project = ProjectStateIdentity {
            project: language_service_project(),
            graph_generation,
            service_generation: number::<ServiceGeneration>(23),
            worker_generation,
        };
        let wrong_sequence = EventEnvelope {
            protocol_version: PROTOCOL_VERSION,
            command_sequence: number::<CommandSequence>(19),
            event: Event::StateAcknowledged(AcknowledgedState::ProjectRefreshed(project)),
        };
        assert!(
            window
                .observe_language_project_event(&wrong_sequence)
                .is_none()
        );
        assert_eq!(window.language_project_context, Some(previous.clone()));
        assert!(window.pending_language_project_refresh.is_some());

        let failed = EventEnvelope {
            protocol_version: PROTOCOL_VERSION,
            command_sequence,
            event: Event::RequestFailed(smudgy_script::language_service::RequestFailure {
                scope: FailureScope::Project(ProjectStateIdentity {
                    graph_generation: number::<GraphGeneration>(19),
                    ..project
                }),
                code: "fixture".to_owned(),
                retryable: true,
                user_message: "retry".to_owned(),
                log_detail: None,
            }),
        };
        let retry = window
            .observe_language_project_event(&failed)
            .expect("matching retryable refresh failure");
        assert_eq!(retry.context, next);
        assert_eq!(retry.retries_remaining, 0);
        assert_eq!(window.language_project_context, Some(previous));
        assert!(window.pending_language_project_refresh.is_none());

        let retry_sequence = number::<CommandSequence>(24);
        window.pending_language_project_refresh = Some(PendingLanguageProjectRefresh {
            context: retry.context.clone(),
            graph_generation,
            command_sequence: retry_sequence,
            retries_remaining: retry.retries_remaining,
            inline_bridge: None,
        });
        let acknowledged = EventEnvelope {
            protocol_version: PROTOCOL_VERSION,
            command_sequence: retry_sequence,
            event: Event::StateAcknowledged(AcknowledgedState::ProjectRefreshed(project)),
        };
        assert!(
            window
                .observe_language_project_event(&acknowledged)
                .is_none()
        );
        assert_eq!(window.language_project_context, Some(retry.context));
        assert!(window.pending_language_project_refresh.is_none());
    }
}

#[cfg(test)]
mod inline_project_tests {
    use smudgy_script::language_service::{PROTOCOL_VERSION, RequestFailure};

    use super::*;

    #[test]
    fn inline_project_sources_share_saved_bodies_and_overlay_the_selected_script() {
        use smudgy_core::models::{ScriptLang, aliases, triggers};

        let mut window = super::super::AutomationsWindow::new(
            iced::window::Id::unique(),
            "inline-shared-state-test".to_owned(),
            crate::cloud_account::test_handles(),
            smudgy_core::session::SessionId::from(1),
        );
        window.scripts.insert(
            "Combat #1".to_owned(),
            super::super::model::Script::Alias(aliases::AliasDefinition {
                pattern: "^combat$".to_owned(),
                script: Some("globalThis.rounds = 1; vars.target = \"goblin\";".to_owned()),
                package: None,
                enabled: true,
                priority: 0,
                fallthrough: true,
                allow_self_match: false,
                language: ScriptLang::JS,
                matcher: None,
                state: Vec::new(),
            }),
        );
        let mut children = std::collections::BTreeMap::new();
        children.insert(
            "Status".to_owned(),
            super::super::model::Script::Trigger(triggers::TriggerDefinition {
                script: Some("vars.target.toUpperCase();".to_owned()),
                package: Some("Nested Folder".to_owned()),
                language: ScriptLang::TS,
                ..triggers::TriggerDefinition::default()
            }),
        );
        window.scripts.insert(
            "Nested Folder".to_owned(),
            super::super::model::Script::Folder(true, children),
        );

        let sources = window
            .language_project_sources(&LanguageProjectContext::Inline)
            .sources;
        assert_eq!(sources.len(), 3);
        let alias = sources
            .iter()
            .find(|source| source.text.contains("globalThis.rounds"))
            .expect("saved alias source");
        assert_eq!(alias.language, Language::JavaScript);
        assert!(alias.uri.contains("/inline/aliases/Combat%20%231.js"));
        let trigger = sources
            .iter()
            .find(|source| source.text.contains("toUpperCase"))
            .expect("saved trigger source");
        assert_eq!(trigger.language, Language::TypeScript);
        assert!(
            trigger
                .uri
                .contains("/inline/triggers/Nested%20Folder/Status.ts")
        );

        window.selection = super::super::Selection::Script(super::super::model::ScriptKey {
            folder_name: None,
            script_name: "Combat #1".to_owned(),
        });
        let descriptor = window.code_document_descriptor(Language::JavaScript, CodeDocument::Alias);
        assert_eq!(descriptor.document.key.document_id, alias.document_id);
        assert_eq!(descriptor.uri, alias.uri);
    }

    fn state_exposure(
        producer: &str,
        handle: Option<&str>,
        name_override: Option<&str>,
        paths: &[&str],
    ) -> smudgy_core::models::state_exposure::StateExposure {
        smudgy_core::models::state_exposure::StateExposure {
            producer: producer.to_owned(),
            handle: handle.map(str::to_owned),
            name_override: name_override.map(str::to_owned),
            paths: paths.iter().map(ToString::to_string).collect(),
        }
    }

    fn wire<T>(value: u64) -> T
    where
        T: TryFrom<u64>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value).expect("valid test wire value")
    }

    fn alias(
        script: &str,
        state: Vec<smudgy_core::models::state_exposure::StateExposure>,
    ) -> super::super::model::Script {
        use smudgy_core::models::{ScriptLang, aliases};

        super::super::model::Script::Alias(aliases::AliasDefinition {
            pattern: "^hp$".to_owned(),
            script: Some(script.to_owned()),
            package: None,
            enabled: true,
            priority: 0,
            fallthrough: true,
            allow_self_match: false,
            language: ScriptLang::TS,
            matcher: None,
            state,
        })
    }

    fn script_key(name: &str) -> super::super::model::ScriptKey {
        super::super::model::ScriptKey {
            folder_name: None,
            script_name: name.to_owned(),
        }
    }

    /// Delivers the exact acknowledgement of the in-flight refresh, as the worker would.
    fn acknowledge_pending_refresh(window: &mut super::super::AutomationsWindow) {
        let pending = window
            .pending_language_project_refresh
            .clone()
            .expect("a project refresh is in flight");
        let acknowledged = EventEnvelope {
            protocol_version: PROTOCOL_VERSION,
            command_sequence: pending.command_sequence,
            event: Event::StateAcknowledged(AcknowledgedState::ProjectRefreshed(
                ProjectStateIdentity {
                    project: language_service_project(),
                    graph_generation: pending.graph_generation,
                    service_generation: wire(1),
                    worker_generation: wire(1),
                },
            )),
        };
        assert!(
            window
                .observe_language_project_event(&acknowledged)
                .is_none()
        );
        assert!(window.pending_language_project_refresh.is_none());
    }

    /// Records an inline refresh carrying `inline_bridge` as in flight at `sequence` and
    /// acknowledges it.
    fn acknowledge_inline_refresh(
        window: &mut super::super::AutomationsWindow,
        inline_bridge: Option<String>,
        sequence: u64,
    ) {
        window.pending_language_project_refresh = Some(PendingLanguageProjectRefresh {
            context: LanguageProjectContext::Inline,
            graph_generation: wire(sequence),
            command_sequence: wire(sequence),
            retries_remaining: 0,
            inline_bridge,
        });
        acknowledge_pending_refresh(window);
    }

    /// The inline project's bridge is the open draft's: each exposed name declared in place of
    /// the user-API member it shadows, and a draft edit makes the installed bridge stale until
    /// a refresh carrying the new one is acknowledged.
    #[test]
    fn inline_project_bridge_declares_the_open_drafts_exposures() {
        use smudgy_core::models::script_typings::{
            language_service_inline_bridge, language_service_inline_bridge_for,
        };

        let mut window = super::super::AutomationsWindow::new(
            iced::window::Id::unique(),
            "inline-bridge-exposures-test".to_owned(),
            crate::cloud_account::test_handles(),
            smudgy_core::session::SessionId::from(1),
        );
        let exposures = vec![
            state_exposure("gmcp", None, None, &["Char.Vitals"]),
            state_exposure("user", Some("foo"), Some("stats"), &["bar"]),
        ];
        window.reset_state_draft(&exposures);
        assert!(
            window.inline_bridge_is_stale(),
            "no inline refresh has carried a bridge yet"
        );

        let project = window.language_project_sources(&LanguageProjectContext::Inline);
        let bridge = project
            .sources
            .iter()
            .find(|source| source.uri == "smudgy-project:///inline/context.d.ts")
            .expect("inline bridge source");
        assert_eq!(bridge.kind, DocumentKind::Generated);
        assert_eq!(bridge.text, language_service_inline_bridge_for(&exposures));
        assert_eq!(project.inline_bridge.as_deref(), Some(bridge.text.as_str()));
        assert!(bridge.text.contains(
            "  const gmcp: { Char: { Vitals: SmudgyStateHop<SmudgyStateHop<GmcpTree, \"Char\">, \
             \"Vitals\"> | undefined } };\n"
        ));
        assert!(bridge.text.contains("  const stats: any;\n"));
        assert!(!bridge.text.contains("const gmcp: SmudgyApi[\"gmcp\"];"));
        assert!(bridge.text.contains("const send: SmudgyApi[\"send\"];"));
        assert!(
            window.inline_bridge_is_stale(),
            "building the refresh's sources installs nothing"
        );
        acknowledge_inline_refresh(&mut window, project.inline_bridge, 40);
        assert!(
            !window.inline_bridge_is_stale(),
            "the acknowledged refresh carries this draft's bridge"
        );

        // An exposure edit changes the bridge; clearing the draft restores the fixed text.
        window.toggle_state_exposure("gmcp", None, "Room.Info");
        assert!(window.inline_bridge_is_stale());
        let project = window.language_project_sources(&LanguageProjectContext::Inline);
        assert!(
            project.sources[0]
                .text
                .contains("Room: { Info: SmudgyStateHop")
        );
        acknowledge_inline_refresh(&mut window, project.inline_bridge, 41);
        assert!(!window.inline_bridge_is_stale());
        window.reset_state_draft(&[]);
        assert!(window.inline_bridge_is_stale());
        let project = window.language_project_sources(&LanguageProjectContext::Inline);
        assert_eq!(project.sources[0].text, language_service_inline_bridge());
        acknowledge_inline_refresh(&mut window, project.inline_bridge, 42);
        assert!(!window.inline_bridge_is_stale());
    }

    /// The installed bridge follows the worker: a refresh's bridge is recorded by that
    /// refresh's acknowledgement alone, the in-flight one is what a draft is measured against
    /// until it fails, and a failed refresh leaves the draft stale, so the next editor binding
    /// or exposure edit sends it again.
    #[test]
    fn inline_bridge_commits_only_on_its_refresh_ack() {
        use smudgy_core::models::script_typings::{
            language_service_inline_bridge, language_service_inline_bridge_for,
        };

        let mut window = super::super::AutomationsWindow::new(
            iced::window::Id::unique(),
            "inline-bridge-commit-test".to_owned(),
            crate::cloud_account::test_handles(),
            smudgy_core::session::SessionId::from(1),
        );
        let exposures = vec![state_exposure("gmcp", None, None, &["Char.Vitals"])];
        let exposing = language_service_inline_bridge_for(&exposures).into_owned();

        acknowledge_inline_refresh(
            &mut window,
            Some(language_service_inline_bridge().to_owned()),
            50,
        );
        assert_eq!(
            window.language_project_context,
            Some(LanguageProjectContext::Inline)
        );
        assert!(!window.inline_bridge_is_stale());
        window.reset_state_draft(&exposures);
        assert!(window.inline_bridge_is_stale());

        let project = window.language_project_sources(&LanguageProjectContext::Inline);
        assert_eq!(project.inline_bridge.as_deref(), Some(exposing.as_str()));
        assert!(
            window.inline_bridge_is_stale(),
            "building the refresh's sources installs nothing"
        );
        let pending = PendingLanguageProjectRefresh {
            context: LanguageProjectContext::Inline,
            graph_generation: wire(51),
            command_sequence: wire(51),
            retries_remaining: 0,
            inline_bridge: project.inline_bridge,
        };
        window.pending_language_project_refresh = Some(pending.clone());
        assert!(
            !window.inline_bridge_is_stale(),
            "the in-flight refresh carries the draft's bridge"
        );
        window.reset_state_draft(&[]);
        assert!(
            window.inline_bridge_is_stale(),
            "the draft no longer matches the in-flight bridge"
        );
        window.reset_state_draft(&exposures);

        let failed = EventEnvelope {
            protocol_version: PROTOCOL_VERSION,
            command_sequence: pending.command_sequence,
            event: Event::RequestFailed(RequestFailure {
                scope: FailureScope::Project(ProjectStateIdentity {
                    project: language_service_project(),
                    graph_generation: wire(50),
                    service_generation: wire(1),
                    worker_generation: wire(1),
                }),
                code: "fixture".to_owned(),
                retryable: false,
                user_message: "failed".to_owned(),
                log_detail: None,
            }),
        };
        assert!(window.observe_language_project_event(&failed).is_none());
        assert!(window.pending_language_project_refresh.is_none());
        assert_eq!(
            window.inline_bridge.as_deref(),
            Some(language_service_inline_bridge()),
            "a failed refresh installs nothing"
        );
        assert!(window.inline_bridge_is_stale());

        window.pending_language_project_refresh = Some(PendingLanguageProjectRefresh {
            graph_generation: wire(52),
            command_sequence: wire(52),
            ..pending
        });
        acknowledge_pending_refresh(&mut window);
        assert_eq!(window.inline_bridge.as_deref(), Some(exposing.as_str()));
        assert!(!window.inline_bridge_is_stale());
    }

    /// A refresh of another context installs no inline bridge.
    #[test]
    fn another_contexts_refresh_carries_no_inline_bridge() {
        let mut window = super::super::AutomationsWindow::new(
            iced::window::Id::unique(),
            "inline-bridge-other-context-test".to_owned(),
            crate::cloud_account::test_handles(),
            smudgy_core::session::SessionId::from(1),
        );
        acknowledge_inline_refresh(
            &mut window,
            Some(smudgy_core::models::script_typings::language_service_inline_bridge().to_owned()),
            60,
        );
        assert!(window.inline_bridge.is_some());

        window.pending_language_project_refresh = Some(PendingLanguageProjectRefresh {
            context: LanguageProjectContext::Modules,
            graph_generation: wire(61),
            command_sequence: wire(61),
            retries_remaining: 0,
            inline_bridge: None,
        });
        assert_eq!(window.installed_inline_bridge(), None);
        acknowledge_pending_refresh(&mut window);
        assert_eq!(
            window.language_project_context,
            Some(LanguageProjectContext::Modules)
        );
        assert_eq!(window.inline_bridge, None);
    }

    /// Binding the editor to another automation re-installs the inline project whenever the
    /// installed bridge is not that automation's: an exposing alias, one exposing nothing, the
    /// first again, and a new alias each send their own bridge, while reopening the automation
    /// whose bridge is installed sends nothing.
    #[test]
    fn binding_another_automation_reinstalls_the_inline_bridge_it_needs() {
        use smudgy_core::models::script_typings::{
            language_service_inline_bridge, language_service_inline_bridge_for,
        };

        let mut window = super::super::AutomationsWindow::new(
            iced::window::Id::unique(),
            "inline-bridge-rebind-test".to_owned(),
            crate::cloud_account::test_handles(),
            smudgy_core::session::SessionId::from(1),
        );
        let exposures = vec![state_exposure("gmcp", None, None, &["Char.Vitals"])];
        let exposing = language_service_inline_bridge_for(&exposures).into_owned();
        let fixed = language_service_inline_bridge();
        window.scripts.insert(
            "Vitals".to_owned(),
            alias("void gmcp.Char.Vitals;\n", exposures),
        );
        window
            .scripts
            .insert("Plain".to_owned(), alias("send(\"hello\");\n", Vec::new()));

        for (name, bridge) in [
            ("Vitals", exposing.as_str()),
            ("Plain", fixed),
            ("Vitals", exposing.as_str()),
        ] {
            let _ = window.open_script(script_key(name));
            let pending = window
                .pending_language_project_refresh
                .as_ref()
                .unwrap_or_else(|| panic!("opening {name} must install its bridge"));
            assert_eq!(pending.context, LanguageProjectContext::Inline);
            assert_eq!(pending.inline_bridge.as_deref(), Some(bridge), "{name}");
            assert!(!window.inline_bridge_is_stale());
            acknowledge_pending_refresh(&mut window);
            assert_eq!(window.inline_bridge.as_deref(), Some(bridge), "{name}");
        }

        let _ = window.open_script(script_key("Vitals"));
        assert!(
            window.pending_language_project_refresh.is_none(),
            "the installed bridge is already this automation's"
        );

        let _ = window.new_alias();
        let pending = window
            .pending_language_project_refresh
            .as_ref()
            .expect("a new alias needs the fixed bridge");
        assert_eq!(pending.inline_bridge.as_deref(), Some(fixed));
    }

    /// Opening an automation installs its own bridge before its body is analyzed, and an
    /// exposure edit re-installs it: the body reading `gmcp.Char.Vitals` is clean while the
    /// path is exposed, fails against the control object once the exposure is removed, and
    /// is clean again once it is back.
    #[test]
    fn exposure_edits_retype_the_open_body() {
        use smudgy_core::models::{ScriptLang, aliases};
        use smudgy_script::language_service::DiagnosticSeverity;

        fn poll_until(
            window: &mut super::super::AutomationsWindow,
            what: &str,
            mut done: impl FnMut(&[Diagnostic]) -> bool,
        ) {
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                let _ = window.poll_language_service();
                let analyzed = window.code_editor.as_ref().is_some_and(|editor| {
                    editor.service_state.is_some()
                        && editor.outstanding.diagnostics.is_none()
                        && editor.service_status() == ServiceStatus::Ready
                        && done(&editor.results().diagnostics)
                });
                if analyzed {
                    return;
                }
                assert!(Instant::now() < deadline, "{what} timed out");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        fn has_error(diagnostics: &[Diagnostic], code: i64) -> bool {
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == Some(DiagnosticCode::Number(code)))
        }

        let mut window = super::super::AutomationsWindow::new(
            iced::window::Id::unique(),
            "inline-bridge-retype-test".to_owned(),
            crate::cloud_account::test_handles(),
            smudgy_core::session::SessionId::from(1),
        );
        window.scripts.insert(
            "Vitals".to_owned(),
            super::super::model::Script::Alias(aliases::AliasDefinition {
                pattern: "^hp$".to_owned(),
                script: Some(
                    "const hp: number | undefined = gmcp.Char.Vitals?.hp;\nvoid hp;\n".to_owned(),
                ),
                package: None,
                enabled: true,
                priority: 0,
                fallthrough: true,
                allow_self_match: false,
                language: ScriptLang::TS,
                matcher: None,
                state: vec![state_exposure("gmcp", None, None, &["Char.Vitals"])],
            }),
        );
        let _ = window.open_script(super::super::model::ScriptKey {
            folder_name: None,
            script_name: "Vitals".to_owned(),
        });
        poll_until(&mut window, "first analysis", |_| true);
        let diagnostics = window
            .code_editor
            .as_ref()
            .unwrap()
            .results()
            .diagnostics
            .clone();
        assert!(
            !diagnostics
                .iter()
                .any(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error),
            "the exposed path must type-check on open: {diagnostics:?}"
        );

        let remove = super::super::Message::RemoveStateExposure {
            producer: "gmcp".to_owned(),
            handle: None,
            path: "Char.Vitals".to_owned(),
        };
        let _ = window.update(remove);
        assert!(window.state_exposures.is_empty());
        poll_until(
            &mut window,
            "analysis without the exposure",
            |diagnostics| has_error(diagnostics, 2339),
        );
        let diagnostics = window
            .code_editor
            .as_ref()
            .unwrap()
            .results()
            .diagnostics
            .clone();
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("'Char'")),
            "`gmcp` must be the control object again: {diagnostics:?}"
        );

        let _ = window.update(super::super::Message::ToggleStateExposure {
            producer: "gmcp".to_owned(),
            handle: None,
            path: "Char.Vitals".to_owned(),
        });
        poll_until(
            &mut window,
            "analysis with the exposure back",
            |diagnostics| !has_error(diagnostics, 2339),
        );
    }
}

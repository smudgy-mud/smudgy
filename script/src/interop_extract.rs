//! Static extraction of interop handle declarations from a package's entry module.
//!
//! The `smudgy:state/` / `smudgy:events/` / `smudgy:procedures/` consumer schemes synthesize
//! their exports from the *producer's source*, never from evaluating it
//! (docs/interop.md §4): a handle is declared as
//! `const promptState = createState<PromptData>('promptState')` with the constructor imported
//! from `smudgy:core`, and the name string in that same declaration is the handle's identity. This
//! module parses the entry source with `deno_ast` and returns those `(kind, name)` pairs plus
//! the exported `export type X = typeof promptState` aliases the typings generator pairs
//! payloads with, and the declared payload-type source the runtime catalogue displays
//! (interop.md §10 tier 3).
//!
//! Extraction is deliberately shallow: top-level `const x = createState('name')` /
//! `createEvent('name')` / `createProcedure(impl)` declarations whose callee is a named import from `smudgy:core` (renames
//! followed) and whose first argument is a string literal. Handles created any other way
//! (dynamically, via a namespace import, in a nested scope) are invisible to static discovery
//! by design — they surface at runtime only (interop.md §4: the runtime catalogue is ground truth).

use std::collections::HashMap;

use deno_ast::SourceRangedForSpanned;
use deno_ast::swc::ast::{
    CallExpr, Callee, Decl, ExportSpecifier, Expr, ImportSpecifier, Lit, Module, ModuleDecl,
    ModuleExportName, ModuleItem, Pat, Stmt, TsEntityName, TsType, TsTypeQueryExpr, VarDecl,
};
use deno_ast::{MediaType, ParseParams};
use deno_core::ModuleSpecifier;

/// Which interop primitive a handle declaration constructs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InteropKind {
    State,
    Event,
    Procedure,
}

impl InteropKind {
    /// The constructor's exported name in `smudgy:core` (also the kind's diagnostic label).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::State => "state",
            Self::Event => "event",
            Self::Procedure => "procedure",
        }
    }
}

/// One statically-discovered handle declaration.
#[derive(Debug, Clone)]
pub struct InteropHandle {
    pub kind: InteropKind,
    /// The handle's identity: the explicit string-literal name when given, else the binding
    /// name (original casing preserved; identity comparisons ASCII-fold, matching the
    /// store's uniform key fold).
    pub name: String,
    /// The const the handle is bound to.
    pub const_name: String,
    /// Whether the declaring const is exported from the module — the typings pipeline
    /// derives consumer payloads from `typeof import(entry)[const_name]` when it is.
    pub exported: bool,
    /// The exported erased type alias (`export type X = typeof const_name`), when present —
    /// the pre-export-handles declaration pattern, kept as the payload source for
    /// module-local handles.
    pub type_alias: Option<String>,
    /// The declared payload type's source text, as display metadata for the runtime
    /// catalogue (interop.md §10 tier 3): the entry-module declaration of the
    /// constructor's type argument when it names one (`createState<PromptData>(…)` →
    /// `export interface PromptData { … }`), else the type argument's own text (inline
    /// object types). Advisory only — never the source of consumer types.
    pub declared_shape: Option<String>,
    /// The payload type's name when the constructor's type argument names a type the entry
    /// itself EXPORTS — the scheme modules re-export it so consumers can import the
    /// author's nicely-named type from the module they already import (interop.md §5).
    pub payload_type_export: Option<String>,
    /// The `/** … */` doc comment on the declaration, propagated onto the generated scheme
    /// exports so hover on the consumer side shows the producer's documentation.
    pub doc: Option<String>,
}

/// The result of statically extracting a module's handle declarations.
#[derive(Debug, Clone, Default)]
pub struct InteropExtraction {
    pub handles: Vec<InteropHandle>,
    /// Case-folded names declared more than once within one kind — a boot/publish diagnostic
    /// (interop.md §4 naming rules). Within a duplicate group the first declaration wins.
    pub duplicates: Vec<String>,
    /// Human-readable export-shape problems (interop.md §4): a handle exported under more
    /// than one name, or under a spelling that isn't its identity. Surfaced at boot and at
    /// publish; the surface must have exactly one spelling per identity.
    pub export_diagnostics: Vec<String>,
}

impl InteropExtraction {
    /// The handles of one kind, in declaration order.
    pub fn of_kind(&self, kind: InteropKind) -> impl Iterator<Item = &InteropHandle> {
        self.handles.iter().filter(move |h| h.kind == kind)
    }
}

/// ASCII case fold — the uniform fold applied everywhere interop names are structural
/// (store keys, event names, scheme path segments; interop.md §2).
#[must_use]
pub fn fold_interop_name(name: &str) -> String {
    name.to_ascii_lowercase()
}

/// Whether `s` is a bare ASCII identifier (`[A-Za-z_$][A-Za-z0-9_$]*`) — the shared
/// spellability test for handle names in emitted TS (type-alias twins, property access) and
/// for telling a named type reference from an inline type's source text.
#[must_use]
pub fn is_ident_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars().enumerate().all(|(i, c)| {
            c == '_' || c == '$' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit())
        })
}

/// The `smudgy:core` constructor names that mint interop handles, and the kind each
/// constructs. The single source for the import-binding match and the injection/scrub
/// pre-scan gates — a new constructor is added here and nowhere else.
pub const HANDLE_CONSTRUCTORS: &[(&str, InteropKind)] = &[
    ("createState", InteropKind::State),
    ("createEvent", InteropKind::Event),
    ("createProcedure", InteropKind::Procedure),
    // A derived value IS published state of this producer (interop.md §4b): consumers read
    // it through smudgy:state/ like any other state handle.
    ("createDerived", InteropKind::State),
];

/// The pre-scan gate the injection and scrub share: parsing is pointless unless the source
/// mentions `smudgy:core` and at least one handle constructor.
fn mentions_handle_constructors(source: &str) -> bool {
    source.contains("smudgy:core")
        && HANDLE_CONSTRUCTORS.iter().any(|(ctor, _)| source.contains(ctor))
}

/// Parse `source` as the media type implied by `specifier` (unknown extensions parse as TS:
/// strictly more permissive than JS). The one parse configuration every consumer of the
/// declaration grammar shares — extraction, name injection, typings, scrub.
fn parse_for_handles(
    specifier: &ModuleSpecifier,
    source: &str,
) -> Result<deno_ast::ParsedSource, Box<deno_ast::ParseDiagnostic>> {
    let media_type = match MediaType::from_specifier(specifier) {
        MediaType::Unknown => MediaType::TypeScript,
        other => other,
    };
    deno_ast::parse_module(ParseParams {
        specifier: specifier.clone(),
        text: source.into(),
        media_type,
        capture_tokens: false,
        scope_analysis: false,
        maybe_syntax: None,
    })
    .map_err(Box::new)
}

/// Which local bindings are the `smudgy:core` interop constructors (renames followed,
/// type-only imports skipped)?
fn constructor_bindings(module: &Module) -> HashMap<String, InteropKind> {
    let mut constructors: HashMap<String, InteropKind> = HashMap::new();
    for item in &module.body {
        let ModuleItem::ModuleDecl(ModuleDecl::Import(import)) = item else {
            continue;
        };
        if import.src.value.as_str() != Some("smudgy:core") || import.type_only {
            continue;
        }
        for spec in &import.specifiers {
            let ImportSpecifier::Named(named) = spec else {
                continue;
            };
            if named.is_type_only {
                continue;
            }
            let imported = match &named.imported {
                Some(ModuleExportName::Ident(ident)) => ident.sym.as_str(),
                Some(ModuleExportName::Str(s)) => s.value.as_str().unwrap_or_default(),
                None => named.local.sym.as_str(),
            };
            let Some((_, kind)) = HANDLE_CONSTRUCTORS.iter().find(|(ctor, _)| *ctor == imported)
            else {
                continue;
            };
            constructors.insert(named.local.sym.to_string(), *kind);
        }
    }
    constructors
}

/// The explicit name argument of a handle constructor call, when there is one. The rule the
/// whole grammar shares (interop.md §4): a string-*literal* first argument is an explicit
/// name; anything else — no arguments, an options bag, `createDerived`'s source handle, a
/// computed expression — means the declaration names itself after its binding.
fn explicit_name_arg(call: &CallExpr) -> Option<String> {
    let first = call.args.first()?;
    if first.spread.is_some() {
        return None;
    }
    let Expr::Lit(Lit::Str(name)) = first.expr.as_ref() else {
        return None;
    };
    name.value.as_str().map(str::to_string)
}

/// Statically extract the interop handle declarations from `source`.
///
/// # Errors
/// Returns the parse diagnostic when `source` is not a syntactically valid module for the
/// media type implied by `specifier`.
pub fn extract_interop_handles(
    specifier: &ModuleSpecifier,
    source: &str,
) -> Result<InteropExtraction, Box<deno_ast::ParseDiagnostic>> {
    let parsed = parse_for_handles(specifier, source)?;
    let deno_ast::ProgramRef::Module(module) = parsed.program_ref() else {
        return Ok(InteropExtraction::default());
    };

    // Pass 1: which local bindings are the smudgy:core constructors (renames followed)?
    let constructors = constructor_bindings(module);
    if constructors.is_empty() {
        return Ok(InteropExtraction::default());
    }

    // Pass 2: top-level `const x = <constructor>('name')` declarations, exported or not.
    let text_info = parsed.text_info_lazy();
    let comments = parsed.comments();
    let mut extraction = InteropExtraction::default();
    let collect_var = |var: &VarDecl,
                       exported: bool,
                       doc: Option<&str>,
                       out: &mut InteropExtraction| {
        for decl in &var.decls {
            let Pat::Ident(binding) = &decl.name else {
                continue;
            };
            let Some(init) = &decl.init else { continue };
            let Expr::Call(call) = init.as_ref() else {
                continue;
            };
            let Callee::Expr(callee) = &call.callee else {
                continue;
            };
            let Expr::Ident(callee_ident) = callee.as_ref() else {
                continue;
            };
            let Some(kind) = constructors.get(callee_ident.sym.as_str()).copied() else {
                continue;
            };
            // A string-literal first argument names the handle explicitly; otherwise the
            // declaration names itself after its binding (the same rule the transpile-time
            // name injection applies, so static discovery and runtime always agree).
            let name = explicit_name_arg(call).unwrap_or_else(|| binding.id.sym.to_string());
            // The constructor's type argument, as source text: a named reference
            // (`createState<PromptData>(…)`) is resolved to its declaration in pass 4; an
            // inline type is its own display shape. JS packages simply have none.
            let declared_shape = call
                .type_args
                .as_ref()
                .and_then(|args| args.params.first())
                .map(|ty| ty.text_fast(text_info).to_string());
            out.handles.push(InteropHandle {
                kind,
                name: name.clone(),
                const_name: binding.id.sym.to_string(),
                exported,
                type_alias: None,
                declared_shape,
                payload_type_export: None,
                doc: doc.map(str::to_string),
            });
        }
    };
    // The declaration's JSDoc block, when one directly precedes it.
    let doc_for = |pos: deno_ast::SourcePos| -> Option<String> {
        let leading = comments.get_leading(pos)?;
        let last = leading.last()?;
        if last.kind == deno_ast::swc::common::comments::CommentKind::Block
            && last.text.starts_with('*')
        {
            Some(format!("/*{}*/", last.text))
        } else {
            None
        }
    };
    for item in &module.body {
        match item {
            ModuleItem::Stmt(Stmt::Decl(Decl::Var(var))) => {
                let doc = doc_for(item.range().start);
                collect_var(var.as_ref(), false, doc.as_deref(), &mut extraction);
            }
            ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => {
                if let Decl::Var(var) = &export.decl {
                    let doc = doc_for(item.range().start);
                    collect_var(var.as_ref(), true, doc.as_deref(), &mut extraction);
                }
            }
            _ => {}
        }
    }

    // Pass 3: pair exported `export type X = typeof <const>` aliases with their handles.
    for item in &module.body {
        let ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) = item else {
            continue;
        };
        let Decl::TsTypeAlias(alias) = &export.decl else {
            continue;
        };
        let TsType::TsTypeQuery(query) = alias.type_ann.as_ref() else {
            continue;
        };
        let TsTypeQueryExpr::TsEntityName(TsEntityName::Ident(target)) = &query.expr_name else {
            continue;
        };
        let target = target.sym.as_str();
        if let Some(handle) = extraction
            .handles
            .iter_mut()
            .find(|h| h.const_name == target)
        {
            handle.type_alias = Some(alias.id.sym.to_string());
        }
    }

    // Pass 4: resolve a named type argument to its entry-module declaration source — display
    // metadata for the runtime catalogue's declared-shape tier (interop.md §10). A
    // name declared elsewhere (an import) stays as the bare reference; advisory either way.
    // When the matched declaration is itself EXPORTED, its name is recorded so the scheme
    // modules can re-export the author's payload type (interop.md §5).
    for handle in &mut extraction.handles {
        let Some(shape) = handle.declared_shape.as_deref() else {
            continue;
        };
        if !is_ident_name(shape) {
            continue;
        }
        let shape = shape.to_string();
        let mut resolved = None;
        for item in &module.body {
            let (decl, exported) = match item {
                ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => (&export.decl, true),
                ModuleItem::Stmt(Stmt::Decl(decl)) => (decl, false),
                _ => continue,
            };
            let matches = match decl {
                Decl::TsInterface(interface) => interface.id.sym.as_str() == shape,
                Decl::TsTypeAlias(alias) => alias.id.sym.as_str() == shape,
                _ => false,
            };
            if matches {
                resolved = Some((item.text_fast(text_info).to_string(), exported));
                break;
            }
        }
        if let Some((resolved, exported)) = resolved {
            handle.declared_shape = Some(resolved);
            if exported {
                handle.payload_type_export = Some(shape);
            }
        }
    }

    // Pass 5: export-shape diagnostics (interop.md §4). Each handle's identity must have
    // exactly one export spelling, and that spelling must BE the identity: a second name
    // (an aliasing `export { vitals as v2 }`, a default export alongside the declaration)
    // or a lone mismatched spelling gives consumers two ways to say one thing — flagged
    // here, surfaced at boot and publish.
    let mut diagnostics: Vec<String> = Vec::new();
    for handle in &mut extraction.handles {
        let mut spellings: Vec<String> = Vec::new();
        if handle.exported {
            spellings.push(handle.const_name.clone());
        }
        for item in &module.body {
            match item {
                ModuleItem::ModuleDecl(ModuleDecl::ExportNamed(named)) if named.src.is_none() => {
                    for spec in &named.specifiers {
                        let ExportSpecifier::Named(n) = spec else { continue };
                        if export_name_text(&n.orig) == handle.const_name {
                            spellings.push(
                                n.exported
                                    .as_ref()
                                    .map_or_else(|| handle.const_name.clone(), export_name_text),
                            );
                        }
                    }
                }
                ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultExpr(def)) => {
                    if let Expr::Ident(ident) = def.expr.as_ref() {
                        if ident.sym.as_str() == handle.const_name {
                            spellings.push("default".to_string());
                        }
                    }
                }
                _ => {}
            }
        }
        if spellings.len() > 1 {
            diagnostics.push(format!(
                "interop handle {:?} is exported under more than one name ({}) — one export spelling per handle",
                handle.name,
                spellings.join(", ")
            ));
        } else if let Some(spelling) = spellings.first() {
            if fold_interop_name(spelling) == fold_interop_name(&handle.name) {
                // A clean named re-export (`export { pinned }`, spelling == identity) is a
                // real export of the declaration: the typings derive its payload from
                // `typeof import(entry)`, the same as an `export const`.
                handle.exported = spelling == &handle.const_name || handle.exported;
            } else {
                diagnostics.push(format!(
                    "interop handle {:?} is exported as {spelling:?} — the export spelling must match the handle's name (rename the binding, or pass the name explicitly to pin the identity)",
                    handle.name
                ));
            }
        }
    }
    extraction.export_diagnostics.extend(diagnostics);

    // Mixed export declaration lists: a handle co-declared with non-handle exports in one
    // `export const` statement loses the WHOLE statement's export-ness on a non-home
    // (code-imported) copy — the scrub removes the `export` keyword, taking the innocent
    // declarators' exports with it. Worth a diagnostic before it worths a link error.
    for item in &module.body {
        let ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) = item else {
            continue;
        };
        let Decl::Var(var) = &export.decl else { continue };
        if var.decls.len() < 2 {
            continue;
        }
        let names: Vec<&str> = var
            .decls
            .iter()
            .filter_map(|d| match &d.name {
                Pat::Ident(b) => Some(b.id.sym.as_str()),
                _ => None,
            })
            .collect();
        let handles: Vec<&&str> = names
            .iter()
            .filter(|n| {
                extraction
                    .handles
                    .iter()
                    .any(|h| h.exported && h.const_name == ***n)
            })
            .collect();
        if !handles.is_empty() && handles.len() < names.len() {
            extraction.export_diagnostics.push(format!(
                "interop handle(s) {} are co-declared with non-handle exports in one `export const` statement — declare handles in their own statement (a code-imported copy loses the whole statement's export-ness)",
                handles.iter().map(|h| format!("{h:?}")).collect::<Vec<_>>().join(", ")
            ));
        }
    }

    // Duplicate names within a kind: first declaration wins; later ones are dropped and the
    // folded name is reported once (the caller surfaces the diagnostic).
    let mut seen: std::collections::HashSet<(InteropKind, String)> = std::collections::HashSet::new();
    let mut duplicated: Vec<String> = Vec::new();
    extraction.handles.retain(|h| {
        let key = (h.kind, fold_interop_name(&h.name));
        if seen.insert(key.clone()) {
            true
        } else {
            if !duplicated.contains(&key.1) {
                duplicated.push(key.1);
            }
            false
        }
    });
    extraction.duplicates = duplicated;

    Ok(extraction)
}

/// Inject inferred handle names at transpile time (interop.md §4): a top-level
/// `const vitals = createState<T>()` becomes `createState<T>("vitals")` before evaluation, so
/// the runtime constructor always receives the name the static grammar inferred. Returns
/// `None` when nothing needed injection (including on parse errors — the transpiler surfaces
/// those properly on its own pass).
///
/// The injection rule is exactly [`explicit_name_arg`]'s complement: a string-literal first
/// argument is left alone; every other call shape gets the binding name spliced in as the
/// first argument. Splices are same-line (a string literal after `(`), so line numbers — and
/// therefore stack traces — are unaffected.
#[must_use]
pub fn inject_inferred_handle_names(specifier: &ModuleSpecifier, source: &str) -> Option<String> {
    // Cheap gate: no constructor spelling or no smudgy:core mention means no injection.
    if !mentions_handle_constructors(source) {
        return None;
    }
    let parsed = parse_for_handles(specifier, source).ok()?;
    let deno_ast::ProgramRef::Module(module) = parsed.program_ref() else {
        return None;
    };
    let constructors = constructor_bindings(module);
    if constructors.is_empty() {
        return None;
    }

    let text_info = parsed.text_info_lazy();
    let program_start = text_info.range().start;
    // (byte index, text to insert) — collected in document order, applied back-to-front.
    let mut insertions: Vec<(usize, String)> = Vec::new();
    let mut collect_var = |var: &VarDecl| {
        for decl in &var.decls {
            let Pat::Ident(binding) = &decl.name else {
                continue;
            };
            let Some(init) = &decl.init else { continue };
            let Expr::Call(call) = init.as_ref() else {
                continue;
            };
            let Callee::Expr(callee) = &call.callee else {
                continue;
            };
            let Expr::Ident(callee_ident) = callee.as_ref() else {
                continue;
            };
            if !constructors.contains_key(callee_ident.sym.as_str()) {
                continue;
            }
            if explicit_name_arg(call).is_some() {
                continue;
            }
            let name_literal = format!("{:?}", binding.id.sym.as_str());
            if let Some(first) = call.args.first() {
                let at = first.range().start.as_byte_index(program_start);
                insertions.push((at, format!("{name_literal}, ")));
            } else {
                // The call ends at `)`; inserting just before it is valid whatever
                // whitespace or comments sit between the parens.
                let at = call.range().end.as_byte_index(program_start) - 1;
                insertions.push((at, name_literal));
            }
        }
    };
    for item in &module.body {
        match item {
            ModuleItem::Stmt(Stmt::Decl(Decl::Var(var))) => collect_var(var.as_ref()),
            ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => {
                if let Decl::Var(var) = &export.decl {
                    collect_var(var.as_ref());
                }
            }
            _ => {}
        }
    }
    if insertions.is_empty() {
        return None;
    }

    let mut out = source.to_string();
    insertions.sort_by_key(|(at, _)| *at);
    for (at, text) in insertions.into_iter().rev() {
        out.insert_str(at, &text);
    }
    Some(out)
}

/// The interop-handle bindings declared at a module's top level (the same detection the
/// extraction, injection, and scrub share).
fn handle_bindings(
    module: &Module,
    constructors: &HashMap<String, InteropKind>,
) -> std::collections::HashSet<String> {
    let mut bindings = std::collections::HashSet::new();
    let collect_var = |var: &VarDecl, out: &mut std::collections::HashSet<String>| {
        for decl in &var.decls {
            let Pat::Ident(binding) = &decl.name else {
                continue;
            };
            let Some(init) = &decl.init else { continue };
            let Expr::Call(call) = init.as_ref() else {
                continue;
            };
            let Callee::Expr(callee) = &call.callee else {
                continue;
            };
            let Expr::Ident(callee_ident) = callee.as_ref() else {
                continue;
            };
            if constructors.contains_key(callee_ident.sym.as_str()) {
                out.insert(binding.id.sym.to_string());
            }
        }
    };
    for item in &module.body {
        match item {
            ModuleItem::Stmt(Stmt::Decl(Decl::Var(var))) => collect_var(var.as_ref(), &mut bindings),
            ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => {
                if let Decl::Var(var) = &export.decl {
                    collect_var(var.as_ref(), &mut bindings);
                }
            }
            _ => {}
        }
    }
    bindings
}

/// The spelling a module-export name binds (`export { x as "y" }` → `y`).
fn export_name_text(name: &ModuleExportName) -> String {
    match name {
        ModuleExportName::Ident(ident) => ident.sym.to_string(),
        ModuleExportName::Str(s) => s.value.as_str().unwrap_or_default().to_string(),
    }
}

/// Remove interop-handle exports from a module's source (interop.md §3). Served on non-home
/// loads: a code-importing consumer's `import { vitals } from "smudgy://…"` then fails at
/// LINK time — loudly — instead of handing out a live producer handle whose writes the home
/// gate would refuse anyway. The declarations themselves survive (the copy's internal code
/// still evaluates; that cost is the pre-existing code-import behavior); only the
/// export-ness is removed. Edits blank with whitespace or preserve newline counts, so line
/// numbers — and therefore stack traces — are unaffected.
///
/// Depth, not hermetic sealing: a handle reachable through a function's return value or an
/// exported container object stays reachable; the home gate (not this scrub) is the
/// security boundary. Returns the scrubbed source plus the removed export names, or `None`
/// when the module exports no handles (including on parse errors — the transpiler surfaces
/// those on its own pass).
#[must_use]
pub fn scrub_handle_exports(
    specifier: &ModuleSpecifier,
    source: &str,
) -> Option<(String, Vec<String>)> {
    if !mentions_handle_constructors(source) {
        return None;
    }
    let parsed = parse_for_handles(specifier, source).ok()?;
    let deno_ast::ProgramRef::Module(module) = parsed.program_ref() else {
        return None;
    };
    let constructors = constructor_bindings(module);
    if constructors.is_empty() {
        return None;
    }
    let bindings = handle_bindings(module, &constructors);
    if bindings.is_empty() {
        return None;
    }

    let text_info = parsed.text_info_lazy();
    let program_start = text_info.range().start;
    let byte_range = |ranged: deno_ast::SourceRange| {
        ranged.start.as_byte_index(program_start)..ranged.end.as_byte_index(program_start)
    };
    // Blank a source range with spaces, preserving its newlines (and nothing else).
    let blanked = |range: &std::ops::Range<usize>| -> String {
        source[range.clone()]
            .chars()
            .map(|c| if c == '\n' || c == '\r' { c } else { ' ' })
            .collect()
    };

    let mut edits: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    let mut removed: Vec<String> = Vec::new();
    for item in &module.body {
        match item {
            // `export const vitals = createState()` — remove just the `export` keyword; the
            // declaration still evaluates. A mixed declaration list loses export-ness for
            // its other declarators too (a discouraged style; the diagnostic names why).
            ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => {
                let Decl::Var(var) = &export.decl else { continue };
                let names: Vec<String> = var
                    .decls
                    .iter()
                    .filter_map(|d| match &d.name {
                        Pat::Ident(b) => Some(b.id.sym.to_string()),
                        _ => None,
                    })
                    .collect();
                if names.iter().any(|n| bindings.contains(n)) {
                    let keyword =
                        export.range().start.as_byte_index(program_start)
                            ..var.range().start.as_byte_index(program_start);
                    edits.push((keyword.clone(), blanked(&keyword)));
                    removed.extend(names);
                }
            }
            // `export { vitals, helper as h }` — drop the handle specifiers, keep survivors.
            ModuleItem::ModuleDecl(ModuleDecl::ExportNamed(named)) if named.src.is_none() => {
                let mut survivors: Vec<String> = Vec::new();
                let mut dropped = false;
                for spec in &named.specifiers {
                    match spec {
                        ExportSpecifier::Named(n) => {
                            let local = export_name_text(&n.orig);
                            if bindings.contains(&local) {
                                dropped = true;
                                removed.push(
                                    n.exported
                                        .as_ref()
                                        .map_or_else(|| local.clone(), export_name_text),
                                );
                            } else {
                                survivors.push(n.text_fast(text_info).to_string());
                            }
                        }
                        other => survivors.push(other.text_fast(text_info).to_string()),
                    }
                }
                if dropped {
                    let range = byte_range(named.range());
                    let newlines = source[range.clone()].matches('\n').count();
                    let mut replacement = if survivors.is_empty() {
                        String::new()
                    } else {
                        format!("export {{ {} }};", survivors.join(", "))
                    };
                    replacement.push_str(&"\n".repeat(newlines));
                    edits.push((range, replacement));
                }
            }
            // `export default vitals` — blank the whole statement.
            ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultExpr(def)) => {
                if let Expr::Ident(ident) = def.expr.as_ref() {
                    if bindings.contains(ident.sym.as_str()) {
                        let range = byte_range(item.range());
                        edits.push((range.clone(), blanked(&range)));
                        removed.push("default".to_string());
                    }
                }
            }
            _ => {}
        }
    }
    if edits.is_empty() {
        return None;
    }

    let mut out = source.to_string();
    edits.sort_by_key(|(range, _)| range.start);
    for (range, replacement) in edits.into_iter().rev() {
        out.replace_range(range, &replacement);
    }
    Some((out, removed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(name: &str, source: &str) -> InteropExtraction {
        let spec = ModuleSpecifier::parse(&format!("file:///{name}")).expect("valid url");
        extract_interop_handles(&spec, source).expect("parses")
    }

    #[test]
    fn extracts_typed_handles_with_aliases() {
        let out = extract(
            "index.ts",
            r#"
            import { createState, createEvent } from "smudgy:core";
            export interface PromptData { hp: number }
            const promptState = createState<PromptData>('promptState');
            const prompt = createEvent<PromptData>('prompt');
            export type PromptState = typeof promptState;
            export type PromptEvent = typeof prompt;
            promptState.set({ hp: 1 });
            "#,
        );
        assert_eq!(out.handles.len(), 2);
        let st = &out.handles[0];
        assert_eq!(st.kind, InteropKind::State);
        assert_eq!(st.name, "promptState");
        assert_eq!(st.const_name, "promptState");
        assert!(!st.exported, "module-local const");
        assert_eq!(st.type_alias.as_deref(), Some("PromptState"));
        assert_eq!(
            st.payload_type_export.as_deref(),
            Some("PromptData"),
            "the type argument names an entry-exported interface"
        );
        let ev = &out.handles[1];
        assert_eq!(ev.kind, InteropKind::Event);
        assert_eq!(ev.name, "prompt");
        assert_eq!(ev.type_alias.as_deref(), Some("PromptEvent"));
        assert!(out.duplicates.is_empty());
    }

    #[test]
    fn captures_exported_flag_and_doc_comments() {
        let out = extract(
            "index.ts",
            r#"
            import { createState } from "smudgy:core";
            interface Hidden { hp: number }
            /** The current vitals reading. */
            export const vitals = createState<Hidden>();
            const internal = createState("internal");
            "#,
        );
        assert_eq!(out.handles.len(), 2);
        let vitals = &out.handles[0];
        assert!(vitals.exported);
        assert_eq!(vitals.doc.as_deref(), Some("/** The current vitals reading. */"));
        assert_eq!(
            vitals.payload_type_export, None,
            "an unexported payload type is not re-exportable"
        );
        assert!(
            vitals.declared_shape.as_deref().unwrap_or_default().contains("interface Hidden"),
            "the catalogue display shape still resolves"
        );
        let internal = &out.handles[1];
        assert!(!internal.exported);
        assert!(internal.doc.is_none());
    }

    #[test]
    fn plain_js_without_type_exports_is_first_class() {
        let out = extract(
            "index.js",
            r#"
            import { createState as makeState } from "smudgy:core";
            const vitals = makeState("vitals");
            export const roster = makeState("roster");
            "#,
        );
        assert_eq!(out.handles.len(), 2);
        assert_eq!(out.handles[0].name, "vitals");
        assert!(out.handles[0].type_alias.is_none());
        // Exported consts are still discovered (hygiene is the author's concern, not ours).
        assert_eq!(out.handles[1].name, "roster");
    }

    #[test]
    fn ignores_lookalikes_not_imported_from_core() {
        let out = extract(
            "index.ts",
            r#"
            import { createEvent } from "smudgy:core";
            function createState(name: string) { return name; }
            const notAHandle = createState("nope");
            const real = createEvent("real");
            "#,
        );
        assert_eq!(out.handles.len(), 1);
        assert_eq!(out.handles[0].name, "real");
    }

    #[test]
    fn infers_names_from_bindings_when_no_literal_argument() {
        let out = extract(
            "index.ts",
            r#"
            import { createState, createEvent, createDerived } from "smudgy:core";
            export const vitals = createState<{ hp: number }>();
            export const prompt = createEvent();
            const roster = createState({ persist: true });
            export const hpPct = createDerived(vitals, (v) => v.hp);
            const computed = createState(pickName());
            "#,
        );
        let names: Vec<(&str, InteropKind)> = out
            .handles
            .iter()
            .map(|h| (h.name.as_str(), h.kind))
            .collect();
        assert_eq!(
            names,
            vec![
                ("vitals", InteropKind::State),
                ("prompt", InteropKind::Event),
                ("roster", InteropKind::State),
                ("hpPct", InteropKind::State),
                // A computed first argument is NOT an explicit name (only a string literal
                // is): the binding names the handle, exactly as the injection will rewrite.
                ("computed", InteropKind::State),
            ],
        );
        assert!(out.duplicates.is_empty());
    }

    #[test]
    fn nested_scope_declarations_stay_invisible() {
        let out = extract(
            "index.ts",
            r#"
            import { createState } from "smudgy:core";
            export function make() {
                const inner = createState();
                return inner;
            }
            "#,
        );
        assert!(out.handles.is_empty(), "only top-level declarations are static");
    }

    #[test]
    fn injects_binding_names_at_the_right_call_shapes() {
        let spec = ModuleSpecifier::parse("file:///index.ts").expect("valid url");
        let source = r#"
import { createState, createEvent, createDerived, createProcedure } from "smudgy:core";
export const vitals = createState<{ hp: number }>();
export const prompt = createEvent(  );
const roster = createState({ persist: true });
export const hpPct = createDerived(vitals, (v) => v.hp);
const refresh = createProcedure((args, sender) => {});
const explicit = createState<{ x: number }>('pinned');
"#;
        let out = inject_inferred_handle_names(&spec, source).expect("injects");
        assert!(out.contains(r#"createState<{ hp: number }>("vitals")"#), "{out}");
        assert!(out.contains(r#"createEvent(  "prompt")"#), "{out}");
        assert!(out.contains(r#"createState("roster", { persist: true })"#), "{out}");
        assert!(out.contains(r#"createDerived("hpPct", vitals, (v) => v.hp)"#), "{out}");
        assert!(
            out.contains(r#"createProcedure("refresh", (args, sender) => {})"#),
            "{out}"
        );
        assert!(out.contains("createState<{ x: number }>('pinned')"), "explicit name untouched: {out}");
        assert_eq!(
            source.lines().count(),
            out.lines().count(),
            "splices never move line numbers"
        );
    }

    #[test]
    fn injection_is_none_when_nothing_applies() {
        let spec = ModuleSpecifier::parse("file:///index.ts").expect("valid url");
        // Explicit names only.
        assert!(
            inject_inferred_handle_names(
                &spec,
                r#"import { createState } from "smudgy:core"; const v = createState("v");"#,
            )
            .is_none()
        );
        // No smudgy:core import at all.
        assert!(
            inject_inferred_handle_names(&spec, "const createState = () => 1; createState();")
                .is_none()
        );
        // Nested scopes are dynamic creation: the runtime demands an explicit name there.
        assert!(
            inject_inferred_handle_names(
                &spec,
                r#"
                import { createState } from "smudgy:core";
                function make() { return createState(); }
                "#,
            )
            .is_none()
        );
        // A parse error is not the injector's problem to report.
        assert!(
            inject_inferred_handle_names(
                &spec,
                r#"import { createState } from "smudgy:core"; const = createState();"#,
            )
            .is_none()
        );
    }

    #[test]
    fn flags_aliased_and_mismatched_handle_exports() {
        let out = extract(
            "index.ts",
            r#"
            import { createState, createEvent } from "smudgy:core";
            export const vitals = createState("vitals");
            export { vitals as v2 };
            const impl = createState("roster");
            export { impl };
            const clean = createEvent();
            export { clean };
            export default clean;
            "#,
        );
        assert_eq!(out.export_diagnostics.len(), 3, "{:#?}", out.export_diagnostics);
        assert!(out.export_diagnostics[0].contains("\"vitals\""), "{:#?}", out.export_diagnostics);
        assert!(
            out.export_diagnostics[0].contains("more than one name"),
            "{:#?}",
            out.export_diagnostics
        );
        assert!(
            out.export_diagnostics[1].contains("exported as \"impl\""),
            "a lone spelling that isn't the identity is flagged: {:#?}",
            out.export_diagnostics
        );
        assert!(
            out.export_diagnostics[2].contains("clean, default"),
            "a named + default pair is two spellings: {:#?}",
            out.export_diagnostics
        );
    }

    #[test]
    fn clean_export_shapes_produce_no_diagnostics() {
        let out = extract(
            "index.ts",
            r#"
            import { createState } from "smudgy:core";
            export const vitals = createState();
            const roster = createState("roster");
            export { roster };
            const hidden = createState("hidden");
            void hidden;
            "#,
        );
        assert!(out.export_diagnostics.is_empty(), "{:#?}", out.export_diagnostics);
    }

    #[test]
    fn scrub_removes_handle_exports_preserving_lines_and_evaluation() {
        let spec = ModuleSpecifier::parse("file:///index.ts").expect("valid url");
        let source = r#"
import { createState, createEvent, createProcedure } from "smudgy:core";
export const vitals = createState<{ hp: number }>("vitals");
const prompt = createEvent("prompt");
export const helper = 42;
export { prompt, helper as h };
export default vitals;
export interface VitalData { hp: number }
"#;
        let (out, removed) = scrub_handle_exports(&spec, source).expect("scrubs");
        // The declaration still evaluates; only export-ness is gone.
        assert!(out.contains(r#" const vitals = createState<{ hp: number }>("vitals");"#), "{out}");
        assert!(!out.contains("export const vitals"), "{out}");
        // Non-handle exports survive, including through a rewritten named-export list.
        assert!(out.contains("export const helper = 42;"), "{out}");
        assert!(out.contains("export { helper as h };"), "{out}");
        assert!(!out.contains("export { prompt"), "{out}");
        assert!(!out.contains("export default vitals"), "{out}");
        // Type exports are untouched.
        assert!(out.contains("export interface VitalData"), "{out}");
        assert_eq!(source.lines().count(), out.lines().count(), "line counts preserved");
        assert_eq!(removed, vec!["vitals".to_string(), "prompt".to_string(), "default".to_string()]);
    }

    #[test]
    fn scrub_is_none_without_handle_exports() {
        let spec = ModuleSpecifier::parse("file:///index.ts").expect("valid url");
        // Handles exist but none are exported: nothing to scrub.
        assert!(
            scrub_handle_exports(
                &spec,
                r#"
                import { createState } from "smudgy:core";
                const vitals = createState("vitals");
                export function read() { return vitals.value; }
                "#,
            )
            .is_none()
        );
        // No handles at all.
        assert!(scrub_handle_exports(&spec, "export const x = 1;").is_none());
    }

    #[test]
    fn injection_works_in_plain_js() {
        let spec = ModuleSpecifier::parse("file:///index.js").expect("valid url");
        let out = inject_inferred_handle_names(
            &spec,
            r#"
import { createState as makeState } from "smudgy:core";
export const vitals = makeState();
"#,
        )
        .expect("injects through renames in JS");
        assert!(out.contains(r#"makeState("vitals")"#), "{out}");
    }

    #[test]
    fn type_only_imports_do_not_bind_constructors() {
        let out = extract(
            "index.ts",
            r#"
            import type { createState } from "smudgy:core";
            const x = createState("phantom");
            "#,
        );
        assert!(out.handles.is_empty());
    }

    #[test]
    fn extracts_procedure_handles_and_declared_shapes() {
        let out = extract(
            "index.ts",
            r#"
            import { createState, createProcedure } from "smudgy:core";
            export interface RefreshRequest { full: boolean }
            const refresh = createProcedure<RefreshRequest>('refreshRequest', (args, sender) => {});
            export type Refresh = typeof refresh;
            const inline = createState<{ hp: number }>('inline');
            const imported = createState<SomewhereElse>('imported');
            "#,
        );
        assert_eq!(out.handles.len(), 3);
        let msg = &out.handles[0];
        assert_eq!(msg.kind, InteropKind::Procedure);
        assert_eq!(msg.name, "refreshRequest");
        assert_eq!(msg.type_alias.as_deref(), Some("Refresh"));
        assert_eq!(
            msg.declared_shape.as_deref(),
            Some("export interface RefreshRequest { full: boolean }"),
            "a named type argument resolves to its entry-module declaration"
        );
        assert_eq!(
            out.handles[1].declared_shape.as_deref(),
            Some("{ hp: number }"),
            "an inline type argument is its own shape text"
        );
        assert_eq!(
            out.handles[2].declared_shape.as_deref(),
            Some("SomewhereElse"),
            "an unresolvable name stays as the bare reference"
        );
    }

    #[test]
    fn duplicate_folded_names_within_a_kind_are_reported_first_wins() {
        let out = extract(
            "index.ts",
            r#"
            import { createState, createEvent } from "smudgy:core";
            const a = createState("Vitals");
            const b = createState("vitals");
            const c = createEvent("vitals"); // different kind: not a duplicate
            "#,
        );
        assert_eq!(out.handles.len(), 2);
        assert_eq!(out.handles[0].name, "Vitals");
        assert_eq!(out.handles[1].kind, InteropKind::Event);
        assert_eq!(out.duplicates, vec!["vitals".to_string()]);
    }
}

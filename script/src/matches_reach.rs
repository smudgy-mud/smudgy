//! Can a handler's source observe the per-fire `matches` / `outer` values?
//!
//! The runtime builds those V8 objects on every match, and a handler that cannot read them
//! pays for them anyway. Answering this once, at registration, lets the fire path skip the
//! work — but only when the answer is certain, so every question here is decided by parsing
//! the source, never by scanning its text. A substring scan gets this wrong in ways that are
//! easy to miss and silent when they land: `globalThis` capitalizes its `This`, `self` is a
//! second spelling of the same object, and `width` contains `with`. The parser knows an
//! identifier from a fragment of one.
//!
//! Both answers over-approximate in the same direction: anything unparseable, and any
//! construct that could reach a name this analysis cannot follow, counts as observing. A
//! wrong `true` costs an object nobody reads; a wrong `false` silently takes one away from a
//! handler that does.

use deno_ast::swc::ast::{ArrowExpr, Expr, FnExpr, Ident, ThisExpr};
use deno_ast::swc::ecma_visit::{Visit, VisitWith};
use deno_ast::{MediaType, ParseParams};
use deno_core::ModuleSpecifier;

/// Identifiers that can reach a global binding without naming it. Each one is either the
/// global object under one of its spellings, or a way to evaluate a name that is not in
/// this source at all. Reaching `matches` through any of them (`self["mat" + "ches"]`)
/// leaves nothing else for the analysis to see, so their mere presence is disqualifying.
const DYNAMIC_GLOBAL_REACH: &[&str] =
    &["globalThis", "self", "window", "global", "eval", "Function"];

/// Set when the walk finds any of the names it was asked to watch for.
struct NameSearch<'a> {
    names: &'a [&'a str],
    /// Whether `this` counts. A classic script's `this` is the global object; a handler
    /// function is called with `this === undefined`, where it reaches no global binding.
    this_reaches_globals: bool,
    found: bool,
}

impl Visit for NameSearch<'_> {
    fn visit_ident(&mut self, ident: &Ident) {
        // Non-computed member properties and object keys are `IdentName`, not `Ident`, so
        // `weapon.matches` and `{ outer: 1 }` never reach this and never force the object.
        if self.names.contains(&ident.sym.as_str()) {
            self.found = true;
        }
        ident.visit_children_with(self);
    }

    fn visit_this_expr(&mut self, this: &ThisExpr) {
        if self.this_reaches_globals {
            self.found = true;
        }
        this.visit_children_with(self);
    }
}

impl NameSearch<'_> {
    fn hit(&self) -> bool {
        self.found
    }
}

fn specifier() -> ModuleSpecifier {
    ModuleSpecifier::parse("file:///smudgy-matches-reach.js").expect("static specifier parses")
}

/// Whether an inline automation body can observe the `matches` or `outer` globals.
///
/// These are ordinary global bindings while the body runs, so naming either identifier
/// observes them, and so does reaching the global object under any of its spellings —
/// including `this`, which at the top level of a classic script *is* that object. The body
/// arrives without the `with` wrapper the engine compiles around it; that wrapper binds
/// neither name and so cannot change this answer.
///
/// This reads one body and does not follow calls out of it, which is sound because nothing
/// a body can call reads the global: the fire path is the only writer of `matches`, and no
/// function in `smudgy.ts` — the whole surface the `with` wrapper puts in scope — reads it.
/// A body therefore reaches these values only by naming a receiver in its own source. The
/// exception is a module that deliberately parks a reader on the global object for a body
/// to call back into by bare name, which reads whatever the last body to write it left
/// there, exactly as it did before.
#[must_use]
pub fn script_can_observe_matches(source: &str) -> bool {
    let Ok(parsed) = deno_ast::parse_script(ParseParams {
        specifier: specifier(),
        text: source.into(),
        media_type: MediaType::JavaScript,
        capture_tokens: false,
        scope_analysis: false,
        maybe_syntax: None,
    }) else {
        // An unparseable body is about to fail to compile anyway; refusing to elide keeps
        // this analysis out of the diagnosis.
        return true;
    };
    let mut names: Vec<&str> = vec!["matches", "outer"];
    names.extend_from_slice(DYNAMIC_GLOBAL_REACH);
    let mut search = NameSearch {
        names: &names,
        this_reaches_globals: true,
        found: false,
    };
    parsed.program_ref().visit_with(&mut search);
    search.hit()
}

/// Whether a handler function can observe the argument the fire path would pass it.
///
/// `matches` reaches a function only as its first argument — never as a global, and never
/// through `this`, which is `undefined` for these calls. So a function that declares no
/// parameter can reach it in exactly two ways: the `arguments` object, and direct `eval`,
/// which can read `arguments` on the callee's behalf. Rest and defaulted parameters need no
/// special case, because they *are* declared parameters here; taking the parameter list
/// from the parsed source rather than from `Function.prototype.length` also means a handler
/// that redefines its own `length` cannot mislead this.
///
/// `source` is the function's own `toString()`. A native or bound function reports
/// `[native code]`, which does not parse, and so keeps its argument.
#[must_use]
pub fn function_can_observe_matches(source: &str) -> bool {
    // `function (m) {}` is not a valid statement, so parse the function in expression
    // position the way `Function.prototype.toString` output is meant to be read.
    let wrapped = format!("({source})");
    let Ok(parsed) = deno_ast::parse_script(ParseParams {
        specifier: specifier(),
        text: wrapped.into(),
        media_type: MediaType::JavaScript,
        capture_tokens: false,
        scope_analysis: false,
        maybe_syntax: None,
    }) else {
        return true;
    };
    let Some(expr) = sole_expression(&parsed) else {
        return true;
    };
    let expr = unparenthesize(expr);
    let declares_parameter = match expr {
        Expr::Arrow(ArrowExpr { params, .. }) => !params.is_empty(),
        Expr::Fn(FnExpr { function, .. }) => !function.params.is_empty(),
        // Anything else is not a function literal this analysis understands.
        _ => return true,
    };
    if declares_parameter {
        return true;
    }
    let names = ["arguments", "eval"];
    let mut search = NameSearch {
        names: &names,
        this_reaches_globals: false,
        found: false,
    };
    expr.visit_with(&mut search);
    search.hit()
}

/// Strip the parentheses the wrapper added (and any the source already had) so the
/// function literal underneath is what gets matched.
fn unparenthesize(expr: &Expr) -> &Expr {
    let mut current = expr;
    while let Expr::Paren(paren) = current {
        current = &paren.expr;
    }
    current
}

/// The single expression of a source parsed as `(<function>)`.
fn sole_expression(parsed: &deno_ast::ParsedSource) -> Option<&Expr> {
    use deno_ast::ProgramRef;
    use deno_ast::swc::ast::Stmt;
    let ProgramRef::Script(script) = parsed.program_ref() else {
        return None;
    };
    match script.body.as_slice() {
        [Stmt::Expr(stmt)] => Some(&*stmt.expr),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handler_that_declares_nothing_and_reaches_nothing_is_elided() {
        assert!(!function_can_observe_matches("() => send(\"flee\")"));
        assert!(!function_can_observe_matches("function () { count++; }"));
        assert!(!function_can_observe_matches(
            "async () => { await tick(); }"
        ));
        // A `this` a sloppy handler resolves to the global object still reaches no
        // argument, so it does not force one.
        assert!(!function_can_observe_matches(
            "function () { this.tick(); }"
        ));
    }

    /// The shape the replay fixture registers 46 times (`countMatch(i)` in
    /// `meta/smudgyvmudlet/trigger-comparison/profile-20260905/full.ts`): a counting
    /// closure that never looks at its argument. The benchmark's whole premise is that
    /// this elides, so pin it rather than infer it.
    #[test]
    fn the_replay_fixture_counting_closure_is_elided() {
        assert!(!function_can_observe_matches(
            "() => {
    if (measuring) {
      matchCounts[index]++;
    }
  }"
        ));
    }

    #[test]
    fn a_handler_that_can_reach_its_argument_keeps_it() {
        assert!(function_can_observe_matches("(m) => m[1]"));
        assert!(function_can_observe_matches("m => m[1]"));
        assert!(function_can_observe_matches("(...rest) => rest[0]"));
        assert!(function_can_observe_matches("(m = undefined) => m"));
        assert!(function_can_observe_matches("({ 1: first }) => first"));
        assert!(function_can_observe_matches(
            "function () { return arguments[0]; }"
        ));
        assert!(function_can_observe_matches(
            "function () { return eval(\"arguments[0]\"); }"
        ));
        // Native and bound functions do not show a body.
        assert!(function_can_observe_matches(
            "function () { [native code] }"
        ));
    }

    #[test]
    fn a_body_that_never_reaches_the_globals_is_elided() {
        assert!(!script_can_observe_matches("count++;"));
        assert!(!script_can_observe_matches("send('flee'); echo('ok');"));
        // A property named `matches` is not the global binding.
        assert!(!script_can_observe_matches("weapon.matches(target);"));
        assert!(!script_can_observe_matches(
            "const o = { outer: 1 }; use(o);"
        ));
        // The substring traps the text scan fell into.
        assert!(!script_can_observe_matches("resize(pane.width);"));
        assert!(!script_can_observe_matches("if (within(a, b)) { go(); }"));
    }

    #[test]
    fn a_body_that_can_reach_the_globals_keeps_them() {
        assert!(script_can_observe_matches("send(matches[1]);"));
        assert!(script_can_observe_matches("echo(outer.name);"));
        assert!(script_can_observe_matches(
            "send(globalThis['mat' + 'ches'][1]);"
        ));
        assert!(script_can_observe_matches("send(self['mat' + 'ches'][1]);"));
        assert!(script_can_observe_matches("send(this['mat' + 'ches'][1]);"));
        assert!(script_can_observe_matches("eval(name);"));
        assert!(script_can_observe_matches("Function('return matches')();"));
        // Unparseable bodies keep them.
        assert!(script_can_observe_matches("this is not javascript {"));
    }
}

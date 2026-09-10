//! The shared object shape, independent of function-argument or script-global delivery.

use super::super::captures::{CaptureView, OuterCaptures};
use deno_core::v8;

/// A bounded cache tied to this exact isolate's lifetime. Large capture counts
/// use temporary keys rather than permanently expanding every isolate's cache.
#[derive(Default)]
pub(super) struct MatchesKeys {
    numeric: Vec<v8::Global<v8::String>>,
    script_name: Option<v8::Global<v8::String>>,
    outer_name: Option<v8::Global<v8::String>>,
}

impl MatchesKeys {
    fn numeric<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        index: usize,
    ) -> v8::Local<'s, v8::String> {
        const CACHE_LIMIT: usize = 32;
        if index >= CACHE_LIMIT {
            return v8::String::new(scope, &index.to_string()).unwrap();
        }
        while self.numeric.len() <= index {
            let name = v8::String::new(scope, &self.numeric.len().to_string()).unwrap();
            self.numeric.push(v8::Global::new(scope, name));
        }
        v8::Local::new(scope, &self.numeric[index])
    }

    pub(super) fn script_name<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
    ) -> v8::Local<'s, v8::String> {
        let name = self.script_name.get_or_insert_with(|| {
            let local = v8::String::new(scope, "matches").unwrap();
            v8::Global::new(scope, local)
        });
        v8::Local::new(scope, &*name)
    }

    pub(super) fn outer_name<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
    ) -> v8::Local<'s, v8::String> {
        let name = self.outer_name.get_or_insert_with(|| {
            let local = v8::String::new(scope, "outer").unwrap();
            v8::Global::new(scope, local)
        });
        v8::Local::new(scope, &*name)
    }
}

/// The `outer` object of an inner trigger: the numbered and named values of the trigger
/// immediately outside, plus the named values of every trigger above that. Nearer levels
/// are written first and a name already present is left alone, so the nearest wins; the
/// registration-time collision check makes that case unreachable in practice.
pub(super) fn materialize_outer<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    outer: &OuterCaptures,
    keys: &mut MatchesKeys,
) -> v8::Local<'s, v8::Object> {
    let object = v8::Object::new(scope);
    for (level, captures) in outer.levels().enumerate() {
        for (index, capture) in captures.iter().enumerate() {
            let named = capture.name.is_some();
            if level > 0 && !named {
                continue;
            }
            let value = v8::String::new(scope, capture.value).unwrap();
            if level == 0 {
                let key = keys.numeric(scope, index);
                object.create_data_property(scope, key.into(), value.into());
            }
            if let Some(name) = capture.name {
                let key = v8::String::new(scope, name).unwrap();
                if level == 0 || !object.has_own_property(scope, key.into()).unwrap_or(false) {
                    object.create_data_property(scope, key.into(), value.into());
                }
            }
        }
    }
    object
}

/// Always produce a fresh ordinary object with own data properties. Assignment
/// APIs can invoke inherited setters and are not interchangeable with this loop.
/// Numeric keys are "0", "1", ...; named keys shadow prototype properties only
/// when present. Both keys for a named group share the same string value.
pub(super) fn materialize_matches<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    matches: CaptureView<'_>,
    keys: &mut MatchesKeys,
) -> v8::Local<'s, v8::Object> {
    let object = v8::Object::new(scope);
    for (index, capture) in matches.iter().enumerate() {
        let value = v8::String::new(scope, capture.value).unwrap();
        let key = keys.numeric(scope, index);
        object.create_data_property(scope, key.into(), value.into());
        if let Some(name) = capture.name {
            let key = v8::String::new(scope, name).unwrap();
            object.create_data_property(scope, key.into(), value.into());
        }
    }
    object
}

/// The `"length"` key for the arity read in [`function_wants_matches`]. Registration is not
/// a hot path, so this builds the string rather than holding another per-isolate cache.
fn length_key<'s>(scope: &mut v8::PinScope<'s, '_>) -> v8::Local<'s, v8::String> {
    v8::String::new(scope, "length").unwrap()
}

/// Whether a registered callable can observe the per-fire `matches`/`outer` objects.
///
/// Building those objects costs a fresh `v8::Object` plus a `v8::String` copy of every
/// capture's text on every fire, and a handler that cannot read them pays that for nothing.
/// The decision is made once, when the callable is registered, and is deliberately
/// one-sided: a callable is exempted only when it *provably* cannot observe the object, so
/// every unproven case keeps today's behavior. Nothing here is a documented contract — a
/// handler that reads `matches` reads exactly what it always did.
///
/// The two delivery shapes need different arguments, so they get different rules.
///
/// A **function handler** receives `matches` as its first argument and nothing else: it is
/// never a global on this path, and `this` is `undefined`. A function that declares no
/// parameters therefore has only two ways to reach the argument — the `arguments` object,
/// and a rest parameter, which reads as `...` in the source. Direct `eval` can reach
/// `arguments` on the callee's behalf, so it disqualifies too. A native or bound function
/// reports `[native code]` instead of a body, which proves nothing, so it also keeps the
/// object.
///
/// The `Function` constructor is *not* an escape here: the function it builds has its own
/// `arguments`, not the caller's.
pub(super) fn function_wants_matches<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    f: v8::Local<'s, v8::Function>,
) -> bool {
    // A declared parameter is the common case and settles it without reading the source.
    // `length` is an own property and configurable, so a handler can lie about it; the
    // source check below independently confirms an empty parameter list, which makes a
    // wrong answer here require two deliberate faults rather than one.
    let length_key = length_key(scope);
    let arity = f
        .get(scope, length_key.into())
        .and_then(|length| length.uint32_value(scope));
    if arity != Some(0) {
        return true;
    }
    // `Function.prototype.toString` is user-reachable and can throw or be replaced. A source
    // this cannot read proves nothing, so an unreadable one keeps the object.
    let Some(source) = f.to_string(scope) else {
        return true;
    };
    let source = source.to_rust_string_lossy(scope);
    // A function that truly declares nothing writes its parameter list as `()`. Anything
    // else — `m =>`, a destructured or defaulted parameter, a rest parameter — leaves no
    // empty pair to find and keeps the object.
    if !source.contains("()") {
        return true;
    }
    ["arguments", "...", "eval", "[native code]"]
        .iter()
        .any(|escape| source.contains(escape))
}

/// The classic-script counterpart of [`function_wants_matches`].
///
/// An inline body reads `matches` and `outer` as globals, so the escapes are wider than a
/// function's: any mention of either identifier, any dynamic global access, and any
/// construct that can evaluate a name this scan cannot see. The body's own source is the
/// whole search space — the `with` wrapper `add_script` adds around it introduces no
/// binding of either name.
///
/// A substring scan over-approximates on purpose. `outerWidth` contains `outer` and keeps
/// the object it does not need, which costs a little work; the reverse mistake would drop
/// an object a body does read, so the scan never has to be clever, only conservative.
///
/// Reading the global under a computed name — `this["mat" + "ches"]` — is the one
/// construction the scan cannot follow, so the receivers that reach it dynamically
/// disqualify a body outright. `this` covers `globalThis` as a substring, and a classic
/// body's `this` is the global object. `eval` and `Function` disqualify for the same
/// reason: the name they evaluate need not appear here.
///
/// A `with` block is not an escape and is not listed: resolving `matches` through one
/// still spells the identifier in this source. Listing it would keep the object for every
/// body containing `width` or `within` and buy nothing.
pub(super) fn script_wants_matches(source: &str) -> bool {
    ["matches", "outer", "this", "eval", "Function"]
        .iter()
        .any(|escape| source.contains(escape))
}

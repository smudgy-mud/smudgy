//! The shared object shape, independent of function-argument or script-global delivery.

use super::super::captures::CaptureView;
use deno_core::v8;

/// A bounded cache tied to this exact isolate's lifetime. Large capture counts
/// use temporary keys rather than permanently expanding every isolate's cache.
#[derive(Default)]
pub(super) struct MatchesKeys {
    numeric: Vec<v8::Global<v8::String>>,
    script_name: Option<v8::Global<v8::String>>,
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

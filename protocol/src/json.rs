use serde_json::Value;

/// Recursive JSON merge: objects compose per key; every other incoming value
/// replaces the value below it.
pub fn deep_merge(base: &mut Value, higher: Value) {
    match (base, higher) {
        (Value::Object(base), Value::Object(higher)) => {
            for (key, value) in higher {
                if let Some(existing) = base.get_mut(&key) {
                    deep_merge(existing, value);
                } else {
                    base.insert(key, value);
                }
            }
        }
        (base, higher) => *base = higher,
    }
}

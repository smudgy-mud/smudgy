pub(crate) use smudgy_protocol::json::deep_merge;

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::deep_merge;

    #[test]
    fn objects_merge_recursively_and_scalars_replace() {
        let mut base = json!({"a": {"b": 1, "c": 2}, "d": 3});
        deep_merge(&mut base, json!({"a": {"b": 4}, "d": [5]}));
        assert_eq!(base, json!({"a": {"b": 4, "c": 2}, "d": [5]}));
    }
}

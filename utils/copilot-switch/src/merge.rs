//! Recursive object merging shared by preset inheritance and output generation.

use serde_json::{Map, Value};

/// Merges `target` over `source`.
///
/// Target values win, but two objects merge recursively. A null target keeps a
/// source object instead of erasing it, while a null target replaces a source
/// scalar or array. Existing keys keep the source's order; keys new in the
/// target are appended in target order.
pub(crate) fn merge_objects(
    source: &Map<String, Value>,
    target: &Map<String, Value>,
) -> Map<String, Value> {
    let mut merged = source.clone();

    for (key, target_value) in target {
        let Some(source_value) = merged.get_mut(key) else {
            merged.insert(key.clone(), target_value.clone());
            continue;
        };

        let replacement = match source_value {
            Value::Object(source_object) => match target_value {
                Value::Null => continue,
                Value::Object(target_object) => {
                    Value::Object(merge_objects(source_object, target_object))
                }
                _ => target_value.clone(),
            },
            _ => target_value.clone(),
        };
        *source_value = replacement;
    }

    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn object(value: Value) -> Map<String, Value> {
        value.as_object().expect("value is an object").clone()
    }

    #[test]
    fn merges_objects_and_replaces_arrays_and_primitives() {
        let source = object(json!({
            "nested": {"kept": true, "overridden": "old"},
            "items": [1, 2],
            "primitive": "old",
            "retained": {"value": 1},
        }));
        let target = object(json!({
            "nested": {"overridden": "new", "added": true},
            "items": [3],
            "primitive": 42,
            "retained": null,
        }));

        let merged = merge_objects(&source, &target);

        assert_eq!(
            Value::Object(merged),
            json!({
                "nested": {"kept": true, "overridden": "new", "added": true},
                "items": [3],
                "primitive": 42,
                "retained": {"value": 1},
            })
        );
    }

    #[test]
    fn null_targets_keep_source_objects_and_replace_scalars() {
        let source = object(json!({"nested": {"kept": true}, "replaced": "source"}));
        let target = object(json!({"nested": null, "replaced": null}));

        let merged = merge_objects(&source, &target);

        assert_eq!(
            Value::Object(merged),
            json!({"nested": {"kept": true}, "replaced": null})
        );
    }

    #[test]
    fn keeps_source_order_then_appends_new_target_keys() {
        let source = object(json!({
            "first": 1,
            "nested": {"a": 1, "b": 2},
        }));
        let target = object(json!({
            "nested": {"b": 3, "c": 4},
            "last": 5,
        }));

        let merged = merge_objects(&source, &target);

        let keys: Vec<&str> = merged.keys().map(String::as_str).collect();
        assert_eq!(keys, ["first", "nested", "last"]);
        let nested = merged["nested"]
            .as_object()
            .expect("nested object should remain");
        let nested_keys: Vec<&str> = nested.keys().map(String::as_str).collect();
        assert_eq!(nested_keys, ["a", "b", "c"]);
    }
}

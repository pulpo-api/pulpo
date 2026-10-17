//! Contract tests: a response body against a JSON Schema (`pm.response.to.have.jsonSchema`),
//! and the schemas of an OpenAPI document turned into plain, self-contained JSON Schema.

use serde_json::{Map, Value};

/// At most this many problems are reported for one body.
const MAX_ERRORS: usize = 10;
/// `$ref`s are inlined up to this depth; deeper (recursive) ones accept anything.
const MAX_REF_DEPTH: usize = 12;

/// The problems of `value` against `schema` (empty when it matches), or why the schema is unusable.
pub fn validate(schema: &Value, value: &Value) -> Result<Vec<String>, String> {
    let validator =
        jsonschema::validator_for(schema).map_err(|e| format!("invalid schema: {e}"))?;
    Ok(validator
        .iter_errors(value)
        .take(MAX_ERRORS)
        .map(|error| {
            let path = error.instance_path().to_string();
            if path.is_empty() {
                error.to_string()
            } else {
                format!("{path}: {error}")
            }
        })
        .collect())
}

/// An OpenAPI schema as standalone JSON Schema: local `$ref`s inlined, `nullable` turned
/// into a `null` type and OpenAPI-only keywords dropped.
pub fn openapi_schema(doc: &Value, schema: &Value) -> Value {
    convert(doc, schema, 0)
}

fn convert(doc: &Value, schema: &Value, depth: usize) -> Value {
    match schema {
        Value::Object(map) => {
            if let Some(reference) = map.get("$ref").and_then(Value::as_str) {
                if depth >= MAX_REF_DEPTH {
                    return Value::Object(Map::new());
                }
                let target = reference
                    .strip_prefix('#')
                    .and_then(|pointer| doc.pointer(pointer));
                return match target {
                    Some(target) => convert(doc, target, depth + 1),
                    None => Value::Object(Map::new()),
                };
            }
            let mut out = Map::new();
            for (key, value) in map {
                match key.as_str() {
                    // Documentation only, or not JSON Schema.
                    "example" | "examples" | "xml" | "externalDocs" | "discriminator"
                    | "nullable" | "readOnly" | "writeOnly" | "deprecated" => {}
                    // Maps of schemas.
                    "properties" | "patternProperties" | "$defs" | "definitions" => {
                        let converted = value
                            .as_object()
                            .map(|m| {
                                m.iter()
                                    .map(|(k, v)| (k.clone(), convert(doc, v, depth)))
                                    .collect()
                            })
                            .unwrap_or_default();
                        out.insert(key.clone(), Value::Object(converted));
                    }
                    // A schema, or a list of schemas.
                    "items"
                    | "additionalProperties"
                    | "not"
                    | "allOf"
                    | "anyOf"
                    | "oneOf"
                    | "prefixItems"
                    | "contains"
                    | "if"
                    | "then"
                    | "else" => {
                        out.insert(key.clone(), convert(doc, value, depth));
                    }
                    _ => {
                        out.insert(key.clone(), value.clone());
                    }
                }
            }
            if map.get("nullable") == Some(&Value::Bool(true)) {
                match out.get("type").cloned() {
                    Some(Value::String(t)) => {
                        out.insert("type".into(), serde_json::json!([t, "null"]));
                    }
                    Some(Value::Array(mut types)) => {
                        types.push(Value::String("null".into()));
                        out.insert("type".into(), Value::Array(types));
                    }
                    _ => {}
                }
                if let Some(Value::Array(values)) = out.get_mut("enum") {
                    values.push(Value::Null);
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(|v| convert(doc, v, depth)).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reports_where_the_body_breaks_the_schema() {
        let schema = json!({
            "type": "object",
            "required": ["id", "items"],
            "properties": {
                "id": { "type": "integer" },
                "items": { "type": "array", "items": { "type": "string" } }
            }
        });
        assert!(validate(&schema, &json!({"id": 1, "items": ["a"]}))
            .unwrap()
            .is_empty());
        let errors = validate(&schema, &json!({"id": "x", "items": ["a", 2]})).unwrap();
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(errors.iter().any(|e| e.starts_with("/id:")), "{errors:?}");
        assert!(
            errors.iter().any(|e| e.starts_with("/items/1:")),
            "{errors:?}"
        );
        assert!(validate(&json!({"type": 12}), &json!(1)).is_err());
    }

    #[test]
    fn turns_openapi_schemas_into_json_schema() {
        let doc = json!({
            "components": { "schemas": {
                "Order": {
                    "type": "object",
                    "required": ["id"],
                    "properties": {
                        "id": { "type": "integer", "example": 3 },
                        "note": { "type": "string", "nullable": true },
                        "parent": { "$ref": "#/components/schemas/Order" }
                    }
                }
            }}
        });
        let schema = openapi_schema(&doc, &json!({ "$ref": "#/components/schemas/Order" }));
        assert_eq!(
            schema["properties"]["note"]["type"],
            json!(["string", "null"])
        );
        assert!(schema["properties"]["id"].get("example").is_none());
        assert_eq!(
            schema["properties"]["parent"]["properties"]["id"]["type"], "integer",
            "refs are inlined (recursion is cut off deeper down)"
        );
        assert!(validate(&schema, &json!({"id": 1, "note": null}))
            .unwrap()
            .is_empty());
        assert!(!validate(&schema, &json!({"note": "x"})).unwrap().is_empty());
    }
}

//! JSONPath (RFC 9535) and jq queries over JSON, for the response filter in the app and for
//! `pm.response.jsonPath()` / `pm.response.jq()` in scripts.

use jaq_core::load::{Arena, File, Loader};
use jaq_core::{data, unwrap_valr, Compiler, Ctx, Vars};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json_path::JsonPath;

/// Most values a query returns: jq can generate endless streams.
const MAX_RESULTS: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QueryLanguage {
    Jsonpath,
    Jq,
}

/// Runs a query over a JSON text. Returns the values it selects (JSONPath) or outputs (jq).
pub fn query(json: &str, language: QueryLanguage, expression: &str) -> Result<Vec<Value>, String> {
    match language {
        QueryLanguage::Jsonpath => json_path(json, expression),
        QueryLanguage::Jq => jq(json, expression),
    }
}

pub fn json_path(json: &str, expression: &str) -> Result<Vec<Value>, String> {
    let value: Value =
        serde_json::from_str(json).map_err(|e| format!("the body is not JSON: {e}"))?;
    let path = JsonPath::parse(expression.trim()).map_err(|e| format!("JSONPath: {e}"))?;
    Ok(path
        .query(&value)
        .all()
        .into_iter()
        .take(MAX_RESULTS)
        .cloned()
        .collect())
}

pub fn jq(json: &str, expression: &str) -> Result<Vec<Value>, String> {
    let input = jaq_json::read::parse_single(json.as_bytes())
        .map_err(|e| format!("the body is not JSON: {e}"))?;
    let program = File {
        code: expression.trim(),
        path: (),
    };
    let defs = jaq_core::defs()
        .chain(jaq_std::defs())
        .chain(jaq_json::defs());
    let funs = jaq_core::funs()
        .chain(jaq_std::funs())
        .chain(jaq_json::funs());
    let loader = Loader::new(defs);
    let arena = Arena::default();
    let modules = loader
        .load(&arena, program)
        .map_err(|errors| format!("jq: cannot parse the filter ({} error(s))", errors.len()))?;
    let filter = Compiler::default()
        .with_funs(funs)
        .compile(modules)
        .map_err(|errors| {
            format!(
                "jq: cannot compile the filter ({} error(s), unknown function?)",
                errors.len()
            )
        })?;
    let ctx = Ctx::<data::JustLut<jaq_json::Val>>::new(&filter.lut, Vars::new([]));
    let mut out = Vec::new();
    for value in filter
        .id
        .run((ctx, input))
        .map(unwrap_valr)
        .take(MAX_RESULTS)
    {
        let value = value.map_err(|e| format!("jq: {e}"))?;
        out.push(serde_json::from_str(&value.to_string()).map_err(|e| format!("jq: {e}"))?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const BODY: &str = r#"{"data": [{"id": 1, "name": "Ana", "tags": ["a"]}, {"id": 2, "name": "Luis", "tags": []}], "total": 2}"#;

    #[test]
    fn json_path_selects_nodes() {
        assert_eq!(json_path(BODY, "$.data[0].id").unwrap(), [json!(1)]);
        assert_eq!(
            json_path(BODY, "$..name").unwrap(),
            [json!("Ana"), json!("Luis")]
        );
        assert_eq!(
            json_path(BODY, "$.data[?@.id > 1].name").unwrap(),
            [json!("Luis")]
        );
        assert!(json_path(BODY, "$.missing").unwrap().is_empty());
        assert!(json_path(BODY, "$[").unwrap_err().starts_with("JSONPath"));
        assert!(json_path("<html>", "$").unwrap_err().contains("not JSON"));
    }

    #[test]
    fn jq_filters_and_transforms() {
        assert_eq!(jq(BODY, ".total").unwrap(), [json!(2)]);
        assert_eq!(
            jq(BODY, ".data[] | .name").unwrap(),
            [json!("Ana"), json!("Luis")]
        );
        assert_eq!(
            jq(BODY, "[.data[] | select(.tags | length > 0) | {id}]").unwrap(),
            [json!([{"id": 1}])]
        );
        assert_eq!(jq(BODY, ".data | map(.id) | add").unwrap(), [json!(3)]);
        assert!(jq(BODY, ".data[").unwrap_err().starts_with("jq"));
        assert!(jq(BODY, "nosuchfunction").unwrap_err().starts_with("jq"));
        assert_eq!(
            jq(BODY, "repeat(1)").unwrap().len(),
            MAX_RESULTS,
            "endless streams are cut"
        );
    }
}

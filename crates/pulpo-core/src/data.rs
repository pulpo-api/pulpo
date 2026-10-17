//! Data files for iterations (`pulpo run --data`, the app's runner): one row per
//! iteration, read from CSV (first line = names) or JSON (an array of objects).

use serde_json::Value;
use std::collections::HashMap;
use std::fmt;
use std::path::Path;

/// One iteration's values, available as `{{name}}` and `pm.iterationData`.
pub type Row = HashMap<String, String>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataError(pub String);

impl fmt::Display for DataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DataError {}

/// Reads a data file; `.json` is JSON, anything else is CSV unless it starts like JSON.
pub fn load_data(path: &Path) -> Result<Vec<Row>, DataError> {
    let text =
        std::fs::read_to_string(path).map_err(|e| DataError(format!("{}: {e}", path.display())))?;
    let is_json = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("json"))
        || text.trim_start().starts_with('[');
    let rows = if is_json {
        parse_json(&text)
    } else {
        parse_csv(&text)
    };
    rows.map_err(|e| DataError(format!("{}: {}", path.display(), e.0)))
}

/// A JSON array of objects. Values that are not strings keep their JSON text (`3`, `true`).
pub fn parse_json(text: &str) -> Result<Vec<Row>, DataError> {
    let value: Value = serde_json::from_str(text).map_err(|e| DataError(e.to_string()))?;
    let Value::Array(items) = value else {
        return Err(DataError("expected an array of objects".into()));
    };
    items
        .into_iter()
        .enumerate()
        .map(|(i, item)| match item {
            Value::Object(map) => Ok(map
                .into_iter()
                .map(|(k, v)| {
                    let text = match v {
                        Value::String(s) => s,
                        Value::Null => String::new(),
                        other => other.to_string(),
                    };
                    (k, text)
                })
                .collect()),
            _ => Err(DataError(format!("item {} is not an object", i + 1))),
        })
        .collect()
}

/// CSV with a header line: commas, `"quoted, fields"` with `""` for a quote, CRLF or LF.
pub fn parse_csv(text: &str) -> Result<Vec<Row>, DataError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let records = csv_records(text)?;
    let mut records = records.into_iter();
    let Some(header) = records.next() else {
        return Ok(Vec::new());
    };
    let names: Vec<String> = header.into_iter().map(|h| h.trim().to_string()).collect();
    let mut rows = Vec::new();
    for (line, record) in records.enumerate() {
        if record.len() == 1 && record[0].is_empty() {
            continue; // blank line
        }
        if record.len() > names.len() {
            return Err(DataError(format!(
                "line {} has {} values but the header has {}",
                line + 2,
                record.len(),
                names.len()
            )));
        }
        let mut row = Row::new();
        for (i, name) in names.iter().enumerate() {
            row.insert(name.clone(), record.get(i).cloned().unwrap_or_default());
        }
        rows.push(row);
    }
    Ok(rows)
}

fn csv_records(text: &str) -> Result<Vec<Vec<String>>, DataError> {
    let mut records = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    let mut at_field_start = true;
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                c => field.push(c),
            }
            continue;
        }
        match c {
            '"' if at_field_start => {
                quoted = true;
                at_field_start = false;
            }
            ',' => {
                record.push(std::mem::take(&mut field));
                at_field_start = true;
            }
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' => {
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
                at_field_start = true;
            }
            c => {
                field.push(c);
                at_field_start = false;
            }
        }
    }
    if quoted {
        return Err(DataError("a quoted value is not closed".into()));
    }
    if !field.is_empty() || !record.is_empty() {
        record.push(field);
        records.push(record);
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pairs: &[(&str, &str)]) -> Row {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn reads_csv_with_quotes_and_windows_line_ends() {
        let rows = parse_csv(
            "\u{feff}user, note\r\nana,\"likes, commas\"\r\n\"bo\",\"say \"\"hi\"\"\"\r\n\r\ncarl\r\n",
        )
        .unwrap();
        assert_eq!(
            rows,
            vec![
                row(&[("user", "ana"), ("note", "likes, commas")]),
                row(&[("user", "bo"), ("note", "say \"hi\"")]),
                row(&[("user", "carl"), ("note", "")]),
            ]
        );
        assert!(parse_csv("a\n1,2\n").is_err());
        assert!(parse_csv("a\n\"open\n").is_err());
    }

    #[test]
    fn reads_json_arrays_of_objects() {
        let rows = parse_json(r#"[{"id": 3, "ok": true, "name": "x", "none": null}]"#).unwrap();
        assert_eq!(
            rows,
            vec![row(&[
                ("id", "3"),
                ("ok", "true"),
                ("name", "x"),
                ("none", "")
            ])]
        );
        assert!(parse_json(r#"{"id": 1}"#).is_err());
        assert!(parse_json("[1]").is_err());
    }

    #[test]
    fn picks_the_format_from_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let csv = dir.path().join("users.csv");
        std::fs::write(&csv, "id\n1\n2\n").unwrap();
        assert_eq!(load_data(&csv).unwrap().len(), 2);
        let json = dir.path().join("users.data");
        std::fs::write(&json, "[{\"id\": 1}]").unwrap();
        assert_eq!(load_data(&json).unwrap()[0]["id"], "1");
        assert!(load_data(&dir.path().join("missing.csv")).is_err());
    }
}

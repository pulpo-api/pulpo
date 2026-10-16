//! `{{variable}}` substitution, including dynamic variables like `{{$guid}}`.

use crate::http::{Auth, Form, FormField, Header, HttpRequest};
use std::collections::{BTreeSet, HashMap};
use std::time::{SystemTime, UNIX_EPOCH};

/// Variables available while resolving a request.
pub type Variables = HashMap<String, String>;

/// Names of the dynamic variables Pulpo generates on every use.
pub const DYNAMIC: [&str; 4] = ["$guid", "$timestamp", "$isoTimestamp", "$randomInt"];

fn dynamic(name: &str) -> Option<String> {
    match name {
        "$guid" => Some(uuid::Uuid::new_v4().to_string()),
        "$timestamp" => Some(unix_seconds().to_string()),
        "$isoTimestamp" => Some(iso_timestamp(unix_millis())),
        "$randomInt" => Some((uuid::Uuid::new_v4().as_u128() % 1001).to_string()),
        _ => None,
    }
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn unix_seconds() -> u128 {
    unix_millis() / 1000
}

/// Formats milliseconds since the Unix epoch as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
fn iso_timestamp(millis: u128) -> String {
    let secs = (millis / 1000) as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}.{:03}Z",
        millis % 1000
    )
}

/// Replaces every `{{name}}` in `text`. Unknown names are left untouched and reported.
pub fn resolve_text(text: &str, vars: &Variables, missing: &mut BTreeSet<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("}}") {
            Some(end) => {
                let name = after[..end].trim();
                match vars.get(name).cloned().or_else(|| dynamic(name)) {
                    Some(value) if !name.is_empty() => out.push_str(&value),
                    _ => {
                        if !name.is_empty() {
                            missing.insert(name.to_string());
                        }
                        out.push_str(&rest[start..start + 2 + end + 2]);
                    }
                }
                rest = &after[end + 2..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// Lists the variable names used in `text`, without resolving them.
pub fn referenced(text: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    resolve_text(text, &Variables::new(), &mut names);
    names.retain(|n| !DYNAMIC.contains(&n.as_str()));
    names
}

/// Resolves the URL, the enabled headers and the body of a request.
/// Returns the names that could not be resolved, if any.
pub fn resolve_request(
    request: &HttpRequest,
    vars: &Variables,
) -> Result<HttpRequest, Vec<String>> {
    let mut missing = BTreeSet::new();
    let resolved = HttpRequest {
        method: request.method,
        url: resolve_text(&request.url, vars, &mut missing),
        headers: request
            .headers
            .iter()
            .map(|h| {
                if h.enabled {
                    Header {
                        name: resolve_text(&h.name, vars, &mut missing),
                        value: resolve_text(&h.value, vars, &mut missing),
                        enabled: true,
                    }
                } else {
                    h.clone()
                }
            })
            .collect(),
        body: request
            .body
            .as_ref()
            .map(|b| resolve_text(b, vars, &mut missing)),
        form: request.form.as_ref().map(|form| Form {
            kind: form.kind,
            fields: form
                .fields
                .iter()
                .map(|f| {
                    if f.enabled {
                        FormField {
                            name: resolve_text(&f.name, vars, &mut missing),
                            value: resolve_text(&f.value, vars, &mut missing),
                            ..f.clone()
                        }
                    } else {
                        f.clone()
                    }
                })
                .collect(),
        }),
        body_file: request
            .body_file
            .as_ref()
            .map(|f| resolve_text(f, vars, &mut missing)),
        timeout_ms: request.timeout_ms,
        disabled_params: request.disabled_params.clone(),
        auth: request.auth.as_ref().map(|auth| match auth {
            Auth::None => Auth::None,
            Auth::Bearer { token } => Auth::Bearer {
                token: resolve_text(token, vars, &mut missing),
            },
            Auth::Basic { username, password } => Auth::Basic {
                username: resolve_text(username, vars, &mut missing),
                password: resolve_text(password, vars, &mut missing),
            },
            Auth::ApiKey {
                key,
                value,
                location,
            } => Auth::ApiKey {
                key: resolve_text(key, vars, &mut missing),
                value: resolve_text(value, vars, &mut missing),
                location: *location,
            },
        }),
        path_params: request
            .path_params
            .iter()
            .map(|p| {
                crate::http::Param::new(p.name.clone(), resolve_text(&p.value, vars, &mut missing))
            })
            .collect(),
    };
    if missing.is_empty() {
        Ok(resolved)
    } else {
        Err(missing.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HttpMethod;

    fn vars(pairs: &[(&str, &str)]) -> Variables {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn replaces_known_variables_and_reports_unknown_ones() {
        let mut missing = BTreeSet::new();
        let text = resolve_text(
            "{{ base }}/orders/{{orderId}}?t={{tenant}}",
            &vars(&[("base", "http://localhost:8888"), ("orderId", "42")]),
            &mut missing,
        );
        assert_eq!(text, "http://localhost:8888/orders/42?t={{tenant}}");
        assert_eq!(missing.into_iter().collect::<Vec<_>>(), ["tenant"]);
    }

    #[test]
    fn leaves_unclosed_and_empty_braces_alone() {
        let mut missing = BTreeSet::new();
        assert_eq!(
            resolve_text("{{}} and {{open", &Variables::new(), &mut missing),
            "{{}} and {{open"
        );
        assert!(missing.is_empty());
    }

    #[test]
    fn generates_dynamic_variables() {
        let mut missing = BTreeSet::new();
        let guid = resolve_text("{{$guid}}", &Variables::new(), &mut missing);
        assert_eq!(guid.len(), 36);
        assert_ne!(
            guid,
            resolve_text("{{$guid}}", &Variables::new(), &mut missing)
        );
        let stamp: u64 = resolve_text("{{$timestamp}}", &Variables::new(), &mut missing)
            .parse()
            .unwrap();
        assert!(stamp > 1_700_000_000);
        let n: u32 = resolve_text("{{$randomInt}}", &Variables::new(), &mut missing)
            .parse()
            .unwrap();
        assert!(n <= 1000);
        assert!(missing.is_empty());
    }

    #[test]
    fn user_variables_override_dynamic_ones() {
        let mut missing = BTreeSet::new();
        assert_eq!(
            resolve_text("{{$guid}}", &vars(&[("$guid", "fixed")]), &mut missing),
            "fixed"
        );
    }

    #[test]
    fn formats_iso_timestamps() {
        assert_eq!(iso_timestamp(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_timestamp(951_782_400_123), "2000-02-29T00:00:00.123Z");
        assert_eq!(iso_timestamp(1_792_108_800_000), "2026-10-16T00:00:00.000Z");
    }

    #[test]
    fn resolves_url_enabled_headers_and_body() {
        let mut request = HttpRequest::new(HttpMethod::Post, "{{base}}/x");
        request.headers = vec![
            Header::new("Authorization", "Bearer {{token}}"),
            Header {
                name: "X-Off".into(),
                value: "{{nope}}".into(),
                enabled: false,
            },
        ];
        request.body = Some(r#"{"tenant":"{{tenant}}"}"#.into());
        let vars = vars(&[
            ("base", "https://api"),
            ("token", "s3cr3t"),
            ("tenant", "test"),
        ]);

        let resolved = resolve_request(&request, &vars).unwrap();

        assert_eq!(resolved.url, "https://api/x");
        assert_eq!(resolved.headers[0].value, "Bearer s3cr3t");
        assert_eq!(resolved.headers[1].value, "{{nope}}");
        assert_eq!(resolved.body.as_deref(), Some(r#"{"tenant":"test"}"#));
    }

    #[test]
    fn fails_with_the_list_of_missing_variables() {
        let request = HttpRequest::new(HttpMethod::Get, "{{base}}/{{a}}/{{base}}");
        assert_eq!(
            resolve_request(&request, &Variables::new()).unwrap_err(),
            ["a", "base"]
        );
    }

    #[test]
    fn lists_referenced_names() {
        let names = referenced("{{a}} {{ b }} {{$guid}} {{a}}");
        assert_eq!(names.into_iter().collect::<Vec<_>>(), ["a", "b"]);
    }
}

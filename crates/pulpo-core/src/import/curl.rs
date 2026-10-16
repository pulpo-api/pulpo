//! curl commands, as copied from a terminal, docs or the browser's "Copy as cURL".

use super::{workspace_path, ImportError, ImportFormat, ImportNode, ImportPlan};
use crate::http::{Auth, Form, FormField, FormKind, Header, HttpRequest};
use crate::HttpMethod;

/// Splits a shell command into words. Handles single and double quotes, backslash
/// escapes, `$'...'` strings and line continuations (`\` on Unix, `^` on Windows cmd).
fn split_words(command: &str) -> Result<Vec<String>, ImportError> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' | '^' if matches!(chars.peek(), Some('\n') | Some('\r')) => {
                while matches!(chars.peek(), Some('\n') | Some('\r')) {
                    chars.next();
                }
            }
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => current.push(c),
                        None => return Err(ImportError::Invalid("unclosed single quote".into())),
                    }
                }
            }
            '$' if chars.peek() == Some(&'\'') => {
                chars.next();
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some('\\') => match chars.next() {
                            Some('n') => current.push('\n'),
                            Some('t') => current.push('\t'),
                            Some('r') => current.push('\r'),
                            Some(other) => current.push(other),
                            None => {
                                return Err(ImportError::Invalid("unclosed $'...' string".into()))
                            }
                        },
                        Some(c) => current.push(c),
                        None => return Err(ImportError::Invalid("unclosed $'...' string".into())),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(e @ ('"' | '\\' | '$' | '`')) => current.push(e),
                            Some('\n') => {}
                            Some(other) => {
                                current.push('\\');
                                current.push(other);
                            }
                            None => {
                                return Err(ImportError::Invalid("unclosed double quote".into()))
                            }
                        },
                        Some(c) => current.push(c),
                        None => return Err(ImportError::Invalid("unclosed double quote".into())),
                    }
                }
            }
            '\\' => {
                in_word = true;
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            c => {
                in_word = true;
                current.push(c);
            }
        }
    }
    if in_word {
        words.push(current);
    }
    Ok(words)
}

fn set_header(headers: &mut Vec<Header>, name: &str, value: String) {
    match headers
        .iter_mut()
        .find(|h| h.name.eq_ignore_ascii_case(name))
    {
        Some(h) => h.value = value,
        None => headers.push(Header::new(name, value)),
    }
}

/// Parses one curl command into a request. Returns warnings for flags that were ignored.
pub fn parse_curl(command: &str) -> Result<(HttpRequest, Vec<String>), ImportError> {
    let words = split_words(command)?;
    let mut args = words.into_iter();
    match args.next().as_deref() {
        Some("curl") => {}
        _ => {
            return Err(ImportError::Invalid(
                "a curl command must start with `curl`".into(),
            ))
        }
    }

    let mut method: Option<String> = None;
    let mut url: Option<String> = None;
    let mut headers: Vec<Header> = Vec::new();
    let mut auth = None;
    let mut data: Vec<String> = Vec::new();
    let mut form: Vec<FormField> = Vec::new();
    let mut body_file: Option<String> = None;
    let mut get_mode = false;
    let mut head = false;
    let mut warnings = Vec::new();

    const IGNORED: [&str; 14] = [
        "-s",
        "--silent",
        "-S",
        "--show-error",
        "-L",
        "--location",
        "-k",
        "--insecure",
        "-v",
        "--verbose",
        "-i",
        "--include",
        "--compressed",
        "-g",
    ];
    const IGNORED_WITH_VALUE: [&str; 6] = [
        "-o",
        "--output",
        "-m",
        "--max-time",
        "--connect-timeout",
        "-w",
    ];

    let next_value = |args: &mut std::vec::IntoIter<String>, flag: &str| {
        args.next()
            .ok_or_else(|| ImportError::Invalid(format!("{flag} needs a value")))
    };

    while let Some(arg) = args.next() {
        // Support --flag=value.
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ => (arg.clone(), None),
        };
        let value = |args: &mut std::vec::IntoIter<String>| match &inline {
            Some(v) => Ok(v.clone()),
            None => next_value(args, &flag),
        };
        match flag.as_str() {
            "-X" | "--request" => method = Some(value(&mut args)?.to_uppercase()),
            "-H" | "--header" => {
                let raw = value(&mut args)?;
                if let Some((name, val)) = raw.split_once(':') {
                    headers.push(Header::new(name.trim(), val.trim()));
                } else {
                    warnings.push(format!("header without a colon ignored: {raw}"));
                }
            }
            "-d" | "--data" | "--data-raw" | "--data-binary" | "--data-ascii" => {
                let body = value(&mut args)?;
                if let Some(file) = body.strip_prefix('@').filter(|_| flag != "--data-raw") {
                    body_file = Some(workspace_path(file, &mut warnings, "curl"));
                } else {
                    data.push(body);
                }
            }
            "--data-urlencode" => data.push(value(&mut args)?),
            "--json" => {
                data.push(value(&mut args)?);
                set_header(&mut headers, "Content-Type", "application/json".into());
                set_header(&mut headers, "Accept", "application/json".into());
            }
            "-u" | "--user" => {
                let credentials = value(&mut args)?;
                let (username, password) = credentials
                    .split_once(':')
                    .unwrap_or((credentials.as_str(), ""));
                auth = Some(Auth::Basic {
                    username: username.to_string(),
                    password: password.to_string(),
                });
            }
            "-A" | "--user-agent" => set_header(&mut headers, "User-Agent", value(&mut args)?),
            "-e" | "--referer" => set_header(&mut headers, "Referer", value(&mut args)?),
            "-b" | "--cookie" => {
                let cookie = value(&mut args)?;
                if cookie.contains('=') {
                    set_header(&mut headers, "Cookie", cookie);
                } else {
                    warnings.push(format!("cookie file {cookie} was not read"));
                }
            }
            "-G" | "--get" => get_mode = true,
            "-I" | "--head" => head = true,
            "--url" => url = Some(value(&mut args)?),
            "-F" | "--form" | "--form-string" => {
                let raw = value(&mut args)?;
                let Some((name, val)) = raw.split_once('=') else {
                    warnings.push(format!("form field without '=' ignored: {raw}"));
                    continue;
                };
                // `name=@path;type=...` uploads a file; `name=<path` sends its content as text.
                let file = (flag != "--form-string")
                    .then(|| val.strip_prefix('@').or_else(|| val.strip_prefix('<')))
                    .flatten();
                form.push(match file {
                    Some(path) => {
                        let path = path.split(';').next().unwrap_or(path);
                        FormField::file(name, workspace_path(path, &mut warnings, "curl"))
                    }
                    None => FormField::text(name, val),
                });
            }
            f if IGNORED.contains(&f) => {}
            f if IGNORED_WITH_VALUE.contains(&f) => {
                value(&mut args)?;
            }
            f if f.starts_with('-') && f.len() > 1 => warnings.push(format!("option {f} ignored")),
            _ => {
                if url.is_none() {
                    url = Some(arg);
                } else {
                    warnings.push(format!("extra argument ignored: {arg}"));
                }
            }
        }
    }

    let mut url = url.ok_or_else(|| ImportError::Invalid("no URL in the curl command".into()))?;
    if !url.contains("://") && !url.starts_with("{{") {
        url = format!("http://{url}");
    }
    let mut body = (!data.is_empty()).then(|| data.join("&"));
    if get_mode {
        if let Some(query) = body.take() {
            url.push(if url.contains('?') { '&' } else { '?' });
            url.push_str(&query);
        }
    }
    let has_body = body.is_some() || body_file.is_some() || !form.is_empty();
    let method_name = method.unwrap_or_else(|| {
        if head {
            "HEAD".into()
        } else if has_body {
            "POST".into()
        } else {
            "GET".into()
        }
    });
    let method: HttpMethod = method_name
        .parse()
        .map_err(|_| ImportError::Invalid(format!("method {method_name} is not supported")))?;
    if body.is_some()
        && body_file.is_none()
        && !headers
            .iter()
            .any(|h| h.name.eq_ignore_ascii_case("content-type"))
    {
        headers.push(Header::new(
            "Content-Type",
            "application/x-www-form-urlencoded",
        ));
    }

    Ok((
        HttpRequest {
            method,
            url,
            headers,
            body,
            form: (!form.is_empty()).then_some(Form {
                kind: FormKind::Multipart,
                fields: form,
            }),
            body_file,
            settings: Default::default(),
            cookies: true,
            disabled_params: Vec::new(),
            path_params: Vec::new(),
            auth,
        },
        warnings,
    ))
}

/// A short name for the request: `POST /orders`.
fn name_for(request: &HttpRequest) -> String {
    let path = request
        .url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(&request.url)
        .split_once('/')
        .map(|(_, p)| format!("/{}", p.split(['?', '#']).next().unwrap_or("")))
        .unwrap_or_else(|| "/".into());
    format!("{} {path}", request.method)
}

pub(super) fn plan(command: &str) -> Result<ImportPlan, ImportError> {
    let (request, warnings) = parse_curl(command)?;
    Ok(ImportPlan {
        format: ImportFormat::Curl,
        folder: None,
        folder_scripts: Default::default(),
        folder_auth: None,
        nodes: vec![ImportNode::Request {
            name: name_for(&request),
            request,
            scripts: Default::default(),
        }],
        environments: Vec::new(),
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_browser_copy_as_curl() {
        let (req, warnings) = parse_curl(
            "curl 'https://api.shop.test/v1/orders?x=1' \\\n  -H 'accept: application/json' \\\n  -H 'authorization: Bearer abc' \\\n  --data-raw '{\"a\":1}' \\\n  --compressed",
        )
        .unwrap();
        assert_eq!(req.method, HttpMethod::Post);
        assert_eq!(req.url, "https://api.shop.test/v1/orders?x=1");
        assert_eq!(req.headers[1], Header::new("authorization", "Bearer abc"));
        assert_eq!(req.body.as_deref(), Some(r#"{"a":1}"#));
        assert!(req.headers.iter().any(|h| h.name == "Content-Type"));
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn parses_windows_cmd_continuations_and_double_quotes() {
        let (req, _) = parse_curl(
            "curl -X PUT \"https://api.test/items/7\" ^\r\n -H \"Content-Type: application/json\" ^\r\n -d \"{\\\"name\\\": \\\"pulpo\\\"}\"",
        )
        .unwrap();
        assert_eq!(req.method, HttpMethod::Put);
        assert_eq!(req.url, "https://api.test/items/7");
        assert_eq!(req.body.as_deref(), Some(r#"{"name": "pulpo"}"#));
        assert_eq!(req.headers.len(), 1);
    }

    #[test]
    fn imports_multipart_fields_and_file_bodies() {
        let (req, warnings) = parse_curl(
            "curl https://x.test/up -F title=Hola -F 'doc=@./files/cv.pdf;type=application/pdf' -F 'raw=@/etc/passwd'",
        )
        .unwrap();
        assert_eq!(req.method, HttpMethod::Post);
        assert_eq!(req.body, None);
        let form = req.form.unwrap();
        assert_eq!(form.kind, FormKind::Multipart);
        assert_eq!(
            form.fields,
            vec![
                FormField::text("title", "Hola"),
                FormField::file("doc", "files/cv.pdf"),
                FormField::file("raw", "files/passwd"),
            ]
        );
        assert!(warnings
            .iter()
            .any(|w| w.contains("copy it to files/passwd")));

        let (req, _) =
            parse_curl("curl -X PUT https://x.test/b --data-binary @data/img.png").unwrap();
        assert_eq!(req.body_file.as_deref(), Some("data/img.png"));
        assert!(!req
            .headers
            .iter()
            .any(|h| h.name.eq_ignore_ascii_case("content-type")));
    }

    #[test]
    fn handles_json_user_get_and_inline_values() {
        let (req, _) =
            parse_curl("curl --json '{\"q\":1}' -u ana:pw --url=https://x.test/s").unwrap();
        assert_eq!(req.method, HttpMethod::Post);
        assert_eq!(req.url, "https://x.test/s");
        assert!(req
            .headers
            .iter()
            .any(|h| h.name == "Content-Type" && h.value == "application/json"));
        assert_eq!(
            req.auth,
            Some(Auth::Basic {
                username: "ana".into(),
                password: "pw".into()
            })
        );
        assert!(!req.headers.iter().any(|h| h.name == "Authorization"));

        let (req, _) = parse_curl("curl -G https://x.test/search -d q=pulpo -d page=2").unwrap();
        assert_eq!(req.method, HttpMethod::Get);
        assert_eq!(req.url, "https://x.test/search?q=pulpo&page=2");
        assert_eq!(req.body, None);
    }

    #[test]
    fn keeps_variables_and_reports_unsupported_flags() {
        let (req, warnings) = parse_curl("curl {{base}}/upload -F file=@a.png --foo").unwrap();
        assert_eq!(req.url, "{{base}}/upload");
        assert_eq!(
            req.form.unwrap().fields,
            vec![FormField::file("file", "a.png")]
        );
        assert_eq!(warnings, vec!["option --foo ignored".to_string()]);
    }

    #[test]
    fn rejects_bad_commands() {
        assert!(parse_curl("wget https://x").is_err());
        assert!(parse_curl("curl -H 'a: b'").is_err());
        assert!(parse_curl("curl 'unclosed").is_err());
    }

    #[test]
    fn names_requests_after_method_and_path() {
        let plan = plan("curl -X DELETE 'https://api.test/v1/orders/9?force=true'").unwrap();
        let ImportNode::Request { name, .. } = &plan.nodes[0] else {
            panic!()
        };
        assert_eq!(name, "DELETE /v1/orders/9");
    }
}

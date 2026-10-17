//! Reports of a run: JUnit XML for CI, a self-contained HTML page for people and a
//! Markdown summary for pull requests (`$GITHUB_STEP_SUMMARY`, PR comments).

use crate::run::RunReport;

/// The current time, for report headers (`Sat, 17 Oct 2026 09:30:00 GMT`).
pub fn now() -> String {
    httpdate::fmt_http_date(std::time::SystemTime::now())
}

/// Totals of a run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    pub requests: usize,
    pub passed: usize,
    pub failed: usize,
    pub tests_passed: usize,
    pub tests_failed: usize,
    pub duration_ms: u64,
    pub iterations: usize,
}

pub fn totals(reports: &[RunReport]) -> Totals {
    let mut t = Totals::default();
    for report in reports {
        t.requests += 1;
        if report.passed() {
            t.passed += 1;
        } else {
            t.failed += 1;
        }
        t.tests_passed += report.tests.iter().filter(|x| x.passed).count();
        t.tests_failed += report.tests.iter().filter(|x| !x.passed).count();
        t.duration_ms += report.response.as_ref().map_or(0, |r| r.duration_ms);
        t.iterations = t.iterations.max(report.iteration + 1);
    }
    t
}

/// The request's name, with its iteration when the run has several.
pub fn label(report: &RunReport, iterations: usize) -> String {
    if iterations > 1 {
        format!("{} [{}/{iterations}]", report.name, report.iteration + 1)
    } else {
        report.name.clone()
    }
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if (c as u32) < 0x20 && !matches!(c, '\n' | '\r' | '\t') => {}
            c => out.push(c),
        }
    }
    out
}

/// JUnit XML: one testsuite per request (and iteration), one testcase per `pm.test`.
/// A request that could not be sent becomes a failed testcase named after it.
pub fn junit_xml(workspace: &str, reports: &[RunReport]) -> String {
    let iterations = totals(reports).iterations;
    let mut suites = String::new();
    let (mut total, mut failures) = (0, 0);
    for report in reports {
        let name = escape(&label(report, iterations));
        let time = report
            .response
            .as_ref()
            .map(|r| r.duration_ms as f64 / 1000.0)
            .unwrap_or(0.0);
        let mut cases = String::new();
        let (mut count, mut failed) = (0, 0);
        if let Some(error) = &report.error {
            count += 1;
            failed += 1;
            cases.push_str(&format!(
                "    <testcase classname=\"{name}\" name=\"{name}\" time=\"0\"><failure message=\"{}\"/></testcase>\n",
                escape(&error.to_string())
            ));
        }
        for test in &report.tests {
            count += 1;
            let test_name = escape(&test.name);
            match &test.error {
                None => cases.push_str(&format!(
                    "    <testcase classname=\"{name}\" name=\"{test_name}\" time=\"{time:.3}\"/>\n"
                )),
                Some(error) => {
                    failed += 1;
                    cases.push_str(&format!(
                        "    <testcase classname=\"{name}\" name=\"{test_name}\" time=\"{time:.3}\"><failure message=\"{}\"/></testcase>\n",
                        escape(error)
                    ));
                }
            }
        }
        if count == 0 {
            count = 1;
            cases.push_str(&format!(
                "    <testcase classname=\"{name}\" name=\"{name}\" time=\"{time:.3}\"/>\n"
            ));
        }
        if !report.script_errors.is_empty() {
            cases.push_str(&format!(
                "    <system-err>{}</system-err>\n",
                escape(&report.script_errors.join("\n"))
            ));
        }
        total += count;
        failures += failed;
        suites.push_str(&format!(
            "  <testsuite name=\"{name}\" tests=\"{count}\" failures=\"{failed}\" time=\"{time:.3}\">\n{cases}  </testsuite>\n"
        ));
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites name=\"{}\" tests=\"{total}\" failures=\"{failures}\">\n{suites}</testsuites>\n",
        escape(workspace)
    )
}

/// Markdown for a pull request: totals, then only what failed (up to 20 requests).
pub fn markdown_summary(workspace: &str, reports: &[RunReport]) -> String {
    let t = totals(reports);
    let mark = if t.failed == 0 { "✅" } else { "❌" };
    let mut out = format!(
        "### {mark} Pulpo · {}\n\n| Requests | Passed | Failed | Tests passed | Tests failed | Time |\n|---:|---:|---:|---:|---:|---:|\n| {} | {} | {} | {} | {} | {} ms |\n",
        workspace.replace('|', "\\|"),
        t.requests,
        t.passed,
        t.failed,
        t.tests_passed,
        t.tests_failed,
        t.duration_ms
    );
    let failed: Vec<&RunReport> = reports.iter().filter(|r| !r.passed()).collect();
    if !failed.is_empty() {
        out.push_str("\n**Failed**\n\n");
        for report in failed.iter().take(20) {
            let status = match (&report.response, &report.error) {
                (Some(r), _) => format!("{}", r.status),
                (None, Some(e)) => e.to_string(),
                (None, None) => String::new(),
            };
            out.push_str(&format!(
                "- **{}** · {}\n",
                label(report, t.iterations).replace('*', "\\*"),
                status.replace('\n', " ")
            ));
            for test in report.tests.iter().filter(|x| !x.passed) {
                out.push_str(&format!(
                    "  - ✗ {}: {}\n",
                    test.name,
                    test.error.as_deref().unwrap_or("").replace('\n', " ")
                ));
            }
            for error in &report.script_errors {
                out.push_str(&format!("  - ! {}\n", error.replace('\n', " ")));
            }
        }
        if failed.len() > 20 {
            out.push_str(&format!("- … and {} more\n", failed.len() - 20));
        }
    }
    out
}

/// A single HTML file with the whole run: totals, every request, its tests and logs.
/// No scripts or external files, so it opens anywhere and can be kept as a CI artifact.
pub fn html_report(workspace: &str, reports: &[RunReport], generated_at: &str) -> String {
    let t = totals(reports);
    let mut rows = String::new();
    for report in reports {
        let ok = report.passed();
        let (status, method_url, time) = match &report.response {
            Some(r) => (
                format!("{} {}", r.status, escape(&r.status_text)),
                r.sent
                    .as_ref()
                    .map(|s| format!("{} {}", escape(&s.method), escape(&s.url)))
                    .unwrap_or_else(|| escape(&r.url)),
                format!("{} ms", r.duration_ms),
            ),
            None => (
                escape(
                    &report
                        .error
                        .as_ref()
                        .map(|e| e.to_string())
                        .unwrap_or_default(),
                ),
                String::new(),
                String::new(),
            ),
        };
        let mut details = String::new();
        for test in &report.tests {
            details.push_str(&format!(
                "<li class=\"{}\">{} {}{}</li>",
                if test.passed { "ok" } else { "ko" },
                if test.passed { "✓" } else { "✗" },
                escape(&test.name),
                test.error
                    .as_deref()
                    .map(|e| format!(" — <span class=\"err\">{}</span>", escape(e)))
                    .unwrap_or_default()
            ));
        }
        for error in &report.script_errors {
            details.push_str(&format!("<li class=\"ko\">! {}</li>", escape(error)));
        }
        for line in &report.logs {
            details.push_str(&format!("<li class=\"log\">&gt; {}</li>", escape(line)));
        }
        let retries = if report.attempts > 1 {
            format!(" <span class=\"tag\">{} attempts</span>", report.attempts)
        } else {
            String::new()
        };
        rows.push_str(&format!(
            "<details{}><summary><span class=\"mark {}\">{}</span> <b>{}</b>{retries} <span class=\"muted\">{method_url}</span> <span class=\"right\">{status} · {time}</span></summary><ul>{details}</ul></details>\n",
            if ok { "" } else { " open" },
            if ok { "ok" } else { "ko" },
            if ok { "✓" } else { "✗" },
            escape(&label(report, t.iterations)),
        ));
    }
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title} · Pulpo run</title>
<style>
:root {{ color-scheme: light dark; --bg: #fff; --fg: #1d1f24; --muted: #6b7280; --line: #e5e7eb; --ok: #15803d; --ko: #b91c1c; }}
@media (prefers-color-scheme: dark) {{ :root {{ --bg: #16181d; --fg: #e5e7eb; --muted: #9ca3af; --line: #2a2e36; --ok: #4ade80; --ko: #f87171; }} }}
body {{ margin: 0 auto; max-width: 1000px; padding: 24px 16px; background: var(--bg); color: var(--fg); font: 14px/1.5 system-ui, sans-serif; }}
h1 {{ font-size: 20px; margin: 0 0 4px; }}
.muted {{ color: var(--muted); }}
.cards {{ display: flex; flex-wrap: wrap; gap: 12px; margin: 16px 0 24px; }}
.card {{ border: 1px solid var(--line); border-radius: 10px; padding: 10px 14px; min-width: 110px; }}
.card b {{ display: block; font-size: 22px; }}
details {{ border-top: 1px solid var(--line); padding: 6px 0; }}
summary {{ cursor: pointer; display: flex; gap: 8px; align-items: baseline; flex-wrap: wrap; }}
.right {{ margin-left: auto; color: var(--muted); font-variant-numeric: tabular-nums; }}
.mark.ok, li.ok {{ color: var(--ok); }}
.mark.ko, li.ko, .err {{ color: var(--ko); }}
li.log {{ color: var(--muted); font-family: ui-monospace, monospace; font-size: 12px; }}
ul {{ margin: 6px 0 6px 20px; padding: 0; list-style: none; }}
.tag {{ font-size: 11px; border: 1px solid var(--line); border-radius: 6px; padding: 0 6px; }}
.muted, summary .muted {{ overflow-wrap: anywhere; }}
</style>
</head>
<body>
<h1>{verdict} {title}</h1>
<div class="muted">Pulpo run · {generated_at}</div>
<div class="cards">
<div class="card"><b>{requests}</b>requests</div>
<div class="card"><b style="color:var(--ok)">{passed}</b>passed</div>
<div class="card"><b style="color:var(--ko)">{failed}</b>failed</div>
<div class="card"><b>{tests_passed}/{tests}</b>tests passed</div>
<div class="card"><b>{iterations}</b>iterations</div>
<div class="card"><b>{duration} ms</b>total time</div>
</div>
{rows}</body>
</html>
"#,
        title = escape(workspace),
        verdict = if t.failed == 0 { "✅" } else { "❌" },
        requests = t.requests,
        passed = t.passed,
        failed = t.failed,
        tests_passed = t.tests_passed,
        tests = t.tests_passed + t.tests_failed,
        iterations = t.iterations.max(1),
        duration = t.duration_ms,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::SendError;
    use crate::script::TestResult;

    fn reports() -> Vec<RunReport> {
        let ok = RunReport {
            name: "List <orders>".into(),
            tests: vec![TestResult {
                name: "status is 200".into(),
                passed: true,
                error: None,
            }],
            ..Default::default()
        };
        let broken = RunReport {
            name: "Create".into(),
            error: Some(SendError::Connect("refused".into())),
            tests: vec![TestResult {
                name: "has id".into(),
                passed: false,
                error: Some("expected: 1 to equal \"2\"".into()),
            }],
            script_errors: vec!["boom".into()],
            iteration: 1,
            ..Default::default()
        };
        vec![ok, broken]
    }

    #[test]
    fn writes_junit_reports() {
        let xml = junit_xml("Shop", &reports());
        assert!(xml.contains("<testsuites name=\"Shop\" tests=\"3\" failures=\"2\">"));
        assert!(xml
            .contains("<testsuite name=\"List &lt;orders&gt; [1/2]\" tests=\"1\" failures=\"0\""));
        assert!(xml.contains("<failure message=\"could not connect: refused\"/>"));
        assert!(xml.contains("<failure message=\"expected: 1 to equal &quot;2&quot;\"/>"));
        assert!(xml.contains("<system-err>boom</system-err>"));
    }

    #[test]
    fn summarizes_failures_in_markdown_and_html() {
        let md = markdown_summary("Shop", &reports());
        assert!(md.starts_with("### ❌ Pulpo · Shop"));
        assert!(md.contains("| 2 | 1 | 1 | 1 | 1 | 0 ms |"));
        assert!(md.contains("- **Create [2/2]** · could not connect: refused"));
        assert!(md.contains("  - ✗ has id: expected: 1 to equal \"2\""));
        assert!(!md.contains("List"), "only failures are listed");

        let html = html_report("Shop", &reports(), "2026-10-17");
        assert!(html.contains("<title>Shop · Pulpo run</title>"));
        assert!(html.contains("List &lt;orders&gt; [1/2]"));
        assert!(!html.contains("<script"), "a static page");
        assert!(html.contains("<details open>"), "failures start open");
    }
}

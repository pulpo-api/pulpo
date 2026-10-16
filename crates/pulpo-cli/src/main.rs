use pulpo_core::collection::{self, TreeNode};
use pulpo_core::environment::{self, Environment, SecretStore};
use pulpo_core::http::{self, Header, HttpRequest};
use pulpo_core::import;
use pulpo_core::keychain::Keychain;
use pulpo_core::run::{self, RunReport};
use pulpo_core::variables::{self, Variables};
use pulpo_core::{HttpMethod, Protocol};
use std::env;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const HELP: &str = "\
pulpo - run Pulpo collections from the terminal

USAGE:
    pulpo <COMMAND>

COMMANDS:
    init [DIR] [--name NAME]
               Turn a folder into a Pulpo workspace (creates pulpo.yaml)
    ls [DIR]   Print the collection tree of a workspace
    fmt [DIR] [--check]
               Rewrite workspace files in canonical form
               (--check only lists them and fails if any would change)
    import <FILE|-> [DIR] [--into FOLDER]
               Import a Postman collection or environment, an OpenAPI 3
               document or a curl command (use - to read it from stdin)
    run [TARGET] [-w DIR] [-e ENV] [--bail] [--junit FILE]
               Run requests with their scripts and tests, in order. TARGET is a
               request or folder path inside the workspace (default: all).
               Exits with 1 if a test fails or a request cannot be sent.
               --junit writes a JUnit XML report for CI.
    env [DIR]  List environments and their variables (secrets are masked)
    secret set <DIR> <ENV> <NAME>
               Store a secret in the OS keychain, reading the value from stdin
    send <METHOD> <URL> [-H 'Name: value']... [-d BODY] [-i] [-w DIR -e ENV]
               Send one HTTP request and print the response body
               -i  also print the status line and headers
               -w/-e  resolve {{variables}} with an environment of a workspace
    version    Print the version
    help       Print this help

Secrets can also come from PULPO_SECRET_<NAME> variables (handy in CI).
More commands (scan, gRPC, WebSocket...) are coming. See https://github.com/pulpo-api/pulpo";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("help") | Some("-h") | Some("--help") => {
            println!("{HELP}");
            ExitCode::SUCCESS
        }
        Some("version") | Some("-V") | Some("--version") => {
            println!("pulpo {}", pulpo_core::VERSION);
            ExitCode::SUCCESS
        }
        Some("send") => match parse_send(&args[1..]) {
            Ok(options) => run_send(&options),
            Err(message) => usage_error(&message),
        },
        Some("init") => run_init(&args[1..]),
        Some("ls") => run_ls(&args[1..]),
        Some("fmt") => run_fmt(&args[1..]),
        Some("env") => run_env(&args[1..]),
        Some("secret") => run_secret(&args[1..]),
        Some("import") => run_import(&args[1..]),
        Some("run") => run_run(&args[1..]),
        Some(other) => usage_error(&format!("unknown command: {other}")),
    }
}

/// Splits `[DIR] [flags]` into the directory (default: current) and the remaining flags.
fn dir_and_flags(args: &[String]) -> (PathBuf, Vec<&str>) {
    let mut dir = PathBuf::from(".");
    let mut flags = Vec::new();
    let mut iter = args.iter().peekable();
    if let Some(first) = iter.peek() {
        if !first.starts_with('-') {
            dir = PathBuf::from(first.as_str());
            iter.next();
        }
    }
    flags.extend(iter.map(String::as_str));
    (dir, flags)
}

fn fail(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("error: {message}");
    ExitCode::FAILURE
}

fn usage_error(message: &str) -> ExitCode {
    eprintln!("{message}\n\n{HELP}");
    ExitCode::from(2)
}

fn run_init(args: &[String]) -> ExitCode {
    let (dir, flags) = dir_and_flags(args);
    let name = match flags.as_slice() {
        [] => default_name(&dir),
        ["--name", name] => name.to_string(),
        _ => return usage_error("init: expected [DIR] [--name NAME]"),
    };
    match collection::init_workspace(&dir, &name) {
        Ok(()) => {
            println!(
                "Created {} in {}",
                collection::WORKSPACE_FILE,
                dir.display()
            );
            ExitCode::SUCCESS
        }
        Err(e) => fail(e),
    }
}

fn default_name(dir: &Path) -> String {
    let absolute = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    absolute
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "My API".to_string())
}

fn run_ls(args: &[String]) -> ExitCode {
    let (dir, flags) = dir_and_flags(args);
    if !flags.is_empty() {
        return usage_error("ls: expected [DIR]");
    }
    match collection::load_workspace(&dir) {
        Ok(workspace) => {
            let mut out = std::io::stdout().lock();
            // A closed pipe (`pulpo ls | head`) is not an error.
            let _ = writeln!(out, "{}", workspace.name)
                .and_then(|_| print_tree(&mut out, &workspace.tree, ""));
            ExitCode::SUCCESS
        }
        Err(e) => fail(e),
    }
}

fn print_tree(out: &mut impl Write, nodes: &[TreeNode], indent: &str) -> std::io::Result<()> {
    for (i, node) in nodes.iter().enumerate() {
        let last = i + 1 == nodes.len();
        let branch = if last { "└── " } else { "├── " };
        match node {
            TreeNode::Folder { name, children, .. } => {
                writeln!(out, "{indent}{branch}{name}/")?;
                let next = format!("{indent}{}", if last { "    " } else { "│   " });
                print_tree(out, children, &next)?;
            }
            TreeNode::Request(entry) => {
                let label = if entry.protocol == Protocol::Http {
                    entry.request.method.to_string()
                } else {
                    entry.protocol.as_str().to_uppercase()
                };
                writeln!(
                    out,
                    "{indent}{branch}{label:<7} {}  {}",
                    entry.name, entry.request.url
                )?;
            }
        }
    }
    Ok(())
}

fn run_fmt(args: &[String]) -> ExitCode {
    let (dir, flags) = dir_and_flags(args);
    let check = match flags.as_slice() {
        [] => false,
        ["--check"] => true,
        _ => return usage_error("fmt: expected [DIR] [--check]"),
    };
    match collection::format_workspace(&dir, check) {
        Ok(changed) if changed.is_empty() => {
            println!("All files are formatted.");
            ExitCode::SUCCESS
        }
        Ok(changed) => {
            for file in &changed {
                println!(
                    "{}{file}",
                    if check { "would format " } else { "formatted " }
                );
            }
            if check {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => fail(e),
    }
}

/// Finds an environment by its name (case-insensitive), file name or id.
fn find_environment(
    dir: &Path,
    wanted: &str,
    store: &dyn SecretStore,
) -> Result<Environment, String> {
    let all = environment::list_environments(dir, store).map_err(|e| e.to_string())?;
    let wanted_lower = wanted.to_lowercase();
    all.iter()
        .find(|e| {
            e.name.to_lowercase() == wanted_lower
                || e.id == wanted
                || e.id == format!("{}/{wanted}.yaml", environment::ENVIRONMENTS_DIR)
        })
        .cloned()
        .ok_or_else(|| {
            let names: Vec<_> = all.iter().map(|e| e.name.as_str()).collect();
            format!(
                "no environment '{wanted}' (available: {})",
                if names.is_empty() {
                    "none".to_string()
                } else {
                    names.join(", ")
                }
            )
        })
}

fn run_env(args: &[String]) -> ExitCode {
    let (dir, flags) = dir_and_flags(args);
    if !flags.is_empty() {
        return usage_error("env: expected [DIR]");
    }
    match environment::list_environments(&dir, &Keychain) {
        Ok(envs) if envs.is_empty() => {
            println!("No environments. Create one in the app or add environments/<name>.yaml.");
            ExitCode::SUCCESS
        }
        Ok(envs) => {
            let mut out = std::io::stdout().lock();
            for env in envs {
                if writeln!(out, "{}  ({})", env.name, env.id).is_err() {
                    break;
                }
                for var in env.variables {
                    let value = match (var.secret, var.has_value) {
                        (true, true) => "•••• (in keychain)".to_string(),
                        (true, false) => "(secret not set)".to_string(),
                        (false, _) => var.value,
                    };
                    let _ = writeln!(out, "    {} = {value}", var.name);
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => fail(e),
    }
}

fn run_secret(args: &[String]) -> ExitCode {
    let [action, dir, env_name, name] = args else {
        return usage_error("secret: expected set <DIR> <ENV> <NAME>");
    };
    if action != "set" {
        return usage_error("secret: expected set <DIR> <ENV> <NAME>");
    }
    let dir = Path::new(dir);
    let mut env = match find_environment(dir, env_name, &Keychain) {
        Ok(env) => env,
        Err(e) => return fail(e),
    };
    let Some(var) = env.variables.iter_mut().find(|v| &v.name == name) else {
        return fail(format!("'{name}' is not a variable of {}", env.name));
    };
    if !var.secret {
        return fail(format!(
            "'{name}' is not marked secret in {}; set `secret: true` first",
            env.id
        ));
    }
    eprintln!("Value for {name} (input is not stored in your shell history):");
    let mut value = String::new();
    if std::io::stdin().lock().read_line(&mut value).is_err() {
        return fail("could not read the value");
    }
    var.value = value.trim_end_matches(['\r', '\n']).to_string();
    if var.value.is_empty() {
        return fail("empty value, nothing stored");
    }
    match environment::save_environment(dir, &env, &Keychain) {
        Ok(()) => {
            println!("Stored {name} for {} in the OS keychain.", env.name);
            ExitCode::SUCCESS
        }
        Err(e) => fail(e),
    }
}

fn run_import(args: &[String]) -> ExitCode {
    let Some(source) = args.first() else {
        return usage_error("import: expected <FILE|-> [DIR] [--into FOLDER]");
    };
    let (dir, flags) = dir_and_flags(&args[1..]);
    let folder = match flags.as_slice() {
        [] => String::new(),
        ["--into", folder] => folder.trim_matches('/').to_string(),
        _ => return usage_error("import: expected <FILE|-> [DIR] [--into FOLDER]"),
    };
    let text = if source == "-" {
        let mut text = String::new();
        if let Err(e) = std::io::stdin().read_to_string(&mut text) {
            return fail(format!("could not read stdin: {e}"));
        }
        text
    } else {
        match std::fs::read_to_string(source) {
            Ok(text) => text,
            Err(e) => return fail(format!("{source}: {e}")),
        }
    };
    if !dir.join(collection::WORKSPACE_FILE).is_file() {
        let name = default_name(&dir);
        if let Err(e) = collection::init_workspace(&dir, &name) {
            return fail(e);
        }
        println!(
            "Created {} in {}",
            collection::WORKSPACE_FILE,
            dir.display()
        );
    }
    let parsed = if source == "-" {
        import::parse(&text)
    } else {
        import::parse_file(Path::new(source))
    };
    let plan = match parsed {
        Ok(plan) => plan,
        Err(e) => return fail(e),
    };
    match import::apply(&dir, &folder, &plan, &Keychain) {
        Ok(summary) => {
            println!(
                "Imported {}: {} requests, {} folders{}{}",
                plan.format.label(),
                summary.requests,
                summary.folders,
                if summary.environments.is_empty() {
                    String::new()
                } else {
                    format!(", environments: {}", summary.environments.join(", "))
                },
                if summary.folder_id.is_empty() {
                    String::new()
                } else {
                    format!(" (in {})", summary.folder_id)
                },
            );
            for warning in &summary.warnings {
                println!("  ! {warning}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => fail(e),
    }
}

fn run_run(args: &[String]) -> ExitCode {
    let mut target = String::new();
    let mut dir = PathBuf::from(".");
    let mut env_name: Option<String> = None;
    let mut bail = false;
    let mut junit: Option<PathBuf> = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "-w" | "--workspace" => match rest.next() {
                Some(d) => dir = PathBuf::from(d),
                None => return usage_error("run: -w needs a DIR"),
            },
            "-e" | "--env" => match rest.next() {
                Some(e) => env_name = Some(e.clone()),
                None => return usage_error("run: -e needs an ENV"),
            },
            "--bail" => bail = true,
            "--junit" => match rest.next() {
                Some(f) => junit = Some(PathBuf::from(f)),
                None => return usage_error("run: --junit needs a FILE"),
            },
            other if !other.starts_with('-') && target.is_empty() => {
                target = other.replace('\\', "/")
            }
            other => return usage_error(&format!("run: unexpected {other}")),
        }
    }
    let workspace = match collection::load_workspace(&dir) {
        Ok(ws) => ws,
        Err(e) => return fail(e),
    };
    let environment_id = match &env_name {
        Some(name) => match find_environment(&dir, name, &Keychain) {
            Ok(env) => Some(env.id),
            Err(e) => return fail(e),
        },
        None => None,
    };
    let requests = run::requests_under(&workspace.tree, &target);
    if requests.is_empty() {
        return fail(format!("nothing to run at '{target}'"));
    }
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => return fail(format!("could not start the runtime: {e}")),
    };

    let (mut passed, mut failed, mut tests_ok, mut tests_ko) = (0, 0, 0, 0);
    let mut reports = Vec::new();
    for entry in &requests {
        let report = runtime.block_on(run::run_entry(
            &dir,
            entry,
            environment_id.as_deref(),
            &Keychain,
        ));
        print_report(&report);
        reports.push(report.clone());
        tests_ok += report.tests.iter().filter(|t| t.passed).count();
        tests_ko += report.tests.iter().filter(|t| !t.passed).count();
        if report.passed() {
            passed += 1;
        } else {
            failed += 1;
            if bail {
                break;
            }
        }
    }
    println!(
        "\n{} requests: {passed} passed, {failed} failed · tests: {tests_ok} passed, {tests_ko} failed",
        passed + failed
    );
    if let Some(path) = junit {
        if let Err(e) = std::fs::write(&path, junit_xml(&workspace.name, &reports)) {
            return fail(format!("{}: {e}", path.display()));
        }
        println!("JUnit report written to {}", path.display());
    }
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn xml_escape(text: &str) -> String {
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

/// JUnit XML: one testsuite per request, one testcase per `pm.test`.
/// A request that could not be sent becomes a failed testcase named after it.
fn junit_xml(workspace: &str, reports: &[RunReport]) -> String {
    let mut suites = String::new();
    let (mut total, mut failures) = (0, 0);
    for report in reports {
        let name = xml_escape(&report.name);
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
                xml_escape(&error.to_string())
            ));
        }
        for test in &report.tests {
            count += 1;
            let test_name = xml_escape(&test.name);
            match &test.error {
                None => cases.push_str(&format!(
                    "    <testcase classname=\"{name}\" name=\"{test_name}\" time=\"{time:.3}\"/>\n"
                )),
                Some(error) => {
                    failed += 1;
                    cases.push_str(&format!(
                        "    <testcase classname=\"{name}\" name=\"{test_name}\" time=\"{time:.3}\"><failure message=\"{}\"/></testcase>\n",
                        xml_escape(error)
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
                xml_escape(&report.script_errors.join("\n"))
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
        xml_escape(workspace)
    )
}

fn print_report(report: &RunReport) {
    let mark = if report.passed() { "✓" } else { "✗" };
    match (&report.response, &report.error) {
        (Some(r), _) => println!(
            "{mark} {}  {} {} · {} ms",
            report.name, r.status, r.status_text, r.duration_ms
        ),
        (None, Some(e)) => println!("{mark} {}  {e}", report.name),
        (None, None) => println!("{mark} {}", report.name),
    }
    for test in &report.tests {
        match &test.error {
            None => println!("    ✓ {}", test.name),
            Some(error) => println!("    ✗ {}: {error}", test.name),
        }
    }
    for error in &report.script_errors {
        println!("    ! {error}");
    }
    for line in &report.logs {
        println!("    > {line}");
    }
}

struct SendOptions {
    request: HttpRequest,
    include_headers: bool,
    workspace: Option<PathBuf>,
    environment: Option<String>,
}

fn parse_send(args: &[String]) -> Result<SendOptions, String> {
    let method: HttpMethod = args
        .first()
        .ok_or("send: missing METHOD")?
        .parse()
        .map_err(|e| format!("send: {e}"))?;
    let url = args.get(1).ok_or("send: missing URL")?;
    let mut options = SendOptions {
        request: HttpRequest::new(method, url.clone()),
        include_headers: false,
        workspace: None,
        environment: None,
    };
    let mut rest = args[2..].iter();
    while let Some(flag) = rest.next() {
        match flag.as_str() {
            "-H" | "--header" => {
                let raw = rest.next().ok_or("send: -H needs 'Name: value'")?;
                let (name, value) = raw
                    .split_once(':')
                    .ok_or_else(|| format!("send: header must be 'Name: value', got '{raw}'"))?;
                options
                    .request
                    .headers
                    .push(Header::new(name.trim(), value.trim()));
            }
            "-d" | "--data" => {
                options.request.body = Some(rest.next().ok_or("send: -d needs a body")?.clone());
            }
            "-i" | "--include" => options.include_headers = true,
            "-w" | "--workspace" => {
                options.workspace = Some(PathBuf::from(rest.next().ok_or("send: -w needs a DIR")?));
            }
            "-e" | "--env" => {
                options.environment = Some(rest.next().ok_or("send: -e needs an ENV")?.clone());
            }
            other => return Err(format!("send: unknown option {other}")),
        }
    }
    if options.environment.is_some() && options.workspace.is_none() {
        options.workspace = Some(PathBuf::from("."));
    }
    Ok(options)
}

fn run_send(options: &SendOptions) -> ExitCode {
    let vars = match (&options.workspace, &options.environment) {
        (Some(dir), Some(name)) => {
            match find_environment(dir, name, &Keychain).and_then(|env| {
                environment::variables_for(dir, &env.id, &Keychain).map_err(|e| e.to_string())
            }) {
                Ok(vars) => vars,
                Err(e) => return fail(e),
            }
        }
        _ => Variables::new(),
    };
    let request = match variables::resolve_request(&options.request, &vars) {
        Ok(request) => request,
        Err(missing) => return fail(format!("unresolved variables: {}", missing.join(", "))),
    };

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => return fail(format!("could not start the runtime: {e}")),
    };
    match runtime.block_on(http::send(&request)) {
        Ok(response) => {
            if options.include_headers {
                println!(
                    "{} {} {}",
                    response.http_version, response.status, response.status_text
                );
                for header in &response.headers {
                    println!("{}: {}", header.name, header.value);
                }
                println!();
            }
            println!("{}", response.body);
            eprintln!(
                "{} {} · {} ms · {} B",
                response.status, response.status_text, response.duration_ms, response.size_bytes
            );
            if response.status >= 400 {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => fail(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_a_full_send_command() {
        let options = parse_send(&args(&[
            "post",
            "{{base}}/orders",
            "-H",
            "X-Tenant-ID: test",
            "-d",
            "{}",
            "-i",
            "-e",
            "Local",
        ]))
        .unwrap();
        assert_eq!(options.request.method, HttpMethod::Post);
        assert_eq!(options.request.url, "{{base}}/orders");
        assert_eq!(
            options.request.headers,
            vec![Header::new("X-Tenant-ID", "test")]
        );
        assert_eq!(options.request.body.as_deref(), Some("{}"));
        assert!(options.include_headers);
        assert_eq!(options.environment.as_deref(), Some("Local"));
        assert_eq!(options.workspace, Some(PathBuf::from(".")));
    }

    #[test]
    fn writes_junit_reports() {
        use pulpo_core::script::TestResult;
        let ok = RunReport {
            id: "a".into(),
            name: "List <orders>".into(),
            tests: vec![
                TestResult {
                    name: "200".into(),
                    passed: true,
                    error: None,
                },
                TestResult {
                    name: "has \"id\"".into(),
                    passed: false,
                    error: Some("expected: x & y".into()),
                },
            ],
            ..Default::default()
        };
        let broken = RunReport {
            id: "b".into(),
            name: "Create".into(),
            error: Some(pulpo_core::http::SendError::Unresolved("base".into())),
            script_errors: vec!["boom".into()],
            ..Default::default()
        };
        let xml = junit_xml("Shop", &[ok, broken]);
        assert!(
            xml.contains(r#"<testsuites name="Shop" tests="3" failures="2">"#),
            "{xml}"
        );
        assert!(xml.contains(r#"name="List &lt;orders&gt;""#));
        assert!(xml.contains(r#"<failure message="expected: x &amp; y"/>"#));
        assert!(xml.contains(r#"<failure message="unresolved variables: base"/>"#));
        assert!(xml.contains("<system-err>boom</system-err>"));
    }

    #[test]
    fn rejects_headers_without_colon() {
        assert!(parse_send(&args(&["GET", "http://x", "-H", "broken"])).is_err());
    }

    #[test]
    fn rejects_unknown_methods() {
        assert!(parse_send(&args(&["FETCH", "http://x"])).is_err());
    }

    #[test]
    fn finds_environments_by_name_or_file() {
        let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/jsonplaceholder");
        let store = environment::MemoryStore::default();
        assert_eq!(
            find_environment(&example, "public", &store).unwrap().name,
            "Public"
        );
        assert_eq!(
            find_environment(&example, "environments/public.yaml", &store)
                .unwrap()
                .name,
            "Public"
        );
        assert!(find_environment(&example, "prod", &store)
            .unwrap_err()
            .contains("Public"));
    }
}

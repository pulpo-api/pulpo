use pulpo_core::codegen;
use pulpo_core::collection::{self, TreeNode};
use pulpo_core::data;
use pulpo_core::environment::{self, Environment, SecretStore};
use pulpo_core::http::{self, Header, HttpRequest};
use pulpo_core::import;
use pulpo_core::keychain::Keychain;
use pulpo_core::load;
use pulpo_core::report;
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
    run [TARGET] [-w DIR] [-e ENV] [options]
               Run requests with their scripts and tests, in order. TARGET is a
               request or folder path inside the workspace (default: all).
               Exits with 1 if a test fails or a request cannot be sent.
               -d, --data FILE      CSV or JSON rows, one iteration each
               -n, --iterations N   run N times (default: one per data row)
               --delay MS           wait between requests
               --retries N          resend after network errors or 5xx
               --bail               stop at the first failure
               --update-snapshots   rewrite snapshots instead of comparing
               --junit FILE         JUnit XML report for CI
               --html FILE          HTML report to open in a browser
               --summary FILE       Markdown summary (for pull requests)
    load [TARGET] [-w DIR] [-e ENV] [-u USERS] [-d DURATION] [--ramp-up SECS]
         [--data FILE]
               Light load test: USERS (default 10, up to 200) run the requests
               in a loop for DURATION (default 30s; 45, 45s or 2m) and the
               percentiles are printed. With --data, user N uses row N.
               Nothing is saved to the workspace.
    code <REQUEST> [-w DIR] [-e ENV] [-l LANG] [--resolve] [--secrets]
               Print the request as code: curl (default), fetch, axios, python,
               java, go, csharp or powershell. Variables stay as {{name}}
               unless --resolve; secrets and values set by scripts only
               with --secrets.
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
        Some("load") => run_load(&args[1..]),
        Some("code") => run_code(&args[1..]),
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

fn run_code(args: &[String]) -> ExitCode {
    let mut target = String::new();
    let mut dir = PathBuf::from(".");
    let mut env_name: Option<String> = None;
    let mut language = codegen::Language::Curl;
    let mut options = codegen::CodeOptions::default();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "-w" | "--workspace" => match rest.next() {
                Some(d) => dir = PathBuf::from(d),
                None => return usage_error("code: -w needs a DIR"),
            },
            "-e" | "--env" => match rest.next() {
                Some(e) => env_name = Some(e.clone()),
                None => return usage_error("code: -e needs an ENV"),
            },
            "-l" | "--lang" => match rest.next().and_then(|l| codegen::Language::parse(l)) {
                Some(l) => language = l,
                None => {
                    let ids: Vec<_> = codegen::Language::ALL.iter().map(|l| l.id()).collect();
                    return usage_error(&format!("code: -l needs one of {}", ids.join(", ")));
                }
            },
            "--resolve" => options.resolve = true,
            "--secrets" => options.include_secrets = true,
            other if !other.starts_with('-') && target.is_empty() => {
                target = other.replace('\\', "/")
            }
            other => return usage_error(&format!("code: unexpected {other}")),
        }
    }
    if target.is_empty() {
        return usage_error("code: expected a REQUEST (its path inside the workspace)");
    }
    let entry = match collection::load_request(&dir, &target) {
        Ok(entry) => entry,
        Err(e) => return fail(e),
    };
    let environment_id = match &env_name {
        Some(name) => match find_environment(&dir, name, &Keychain) {
            Ok(env) => Some(env.id),
            Err(e) => return fail(e),
        },
        None => None,
    };
    match codegen::for_entry(
        &dir,
        &entry,
        environment_id.as_deref(),
        &Keychain,
        language,
        options,
    ) {
        Ok(code) => {
            let _ = writeln!(std::io::stdout(), "{code}");
            ExitCode::SUCCESS
        }
        Err(e) => fail(e),
    }
}

/// Parses `"30"`, `"30s"`, `"2m"` into seconds.
fn parse_seconds(text: &str) -> Option<u64> {
    let text = text.trim();
    if let Some(minutes) = text.strip_suffix('m') {
        return minutes.parse::<u64>().ok().map(|m| m * 60);
    }
    text.strip_suffix('s').unwrap_or(text).parse().ok()
}

fn number<T: std::str::FromStr>(value: Option<&String>, flag: &str) -> Result<T, String> {
    value
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| format!("{flag} needs a number"))
}

fn next_path(rest: &mut std::slice::Iter<'_, String>, flag: &str) -> Result<PathBuf, String> {
    rest.next()
        .map(PathBuf::from)
        .ok_or_else(|| format!("run: {flag} needs a FILE"))
}

struct RunArgs {
    target: String,
    dir: PathBuf,
    env_name: Option<String>,
    bail: bool,
    junit: Option<PathBuf>,
    html: Option<PathBuf>,
    summary: Option<PathBuf>,
    data: Option<PathBuf>,
    iterations: Option<usize>,
    delay_ms: u64,
    retries: u32,
    update_snapshots: bool,
}

fn parse_run(args: &[String]) -> Result<RunArgs, String> {
    let mut out = RunArgs {
        target: String::new(),
        dir: PathBuf::from("."),
        env_name: None,
        bail: false,
        junit: None,
        html: None,
        summary: None,
        data: None,
        iterations: None,
        delay_ms: 0,
        retries: 0,
        update_snapshots: false,
    };
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "-w" | "--workspace" => out.dir = next_path(&mut rest, "-w")?,
            "-e" | "--env" => {
                out.env_name = Some(rest.next().ok_or("run: -e needs an ENV")?.clone())
            }
            "--bail" => out.bail = true,
            "--junit" => out.junit = Some(next_path(&mut rest, "--junit")?),
            "--html" => out.html = Some(next_path(&mut rest, "--html")?),
            "--summary" => out.summary = Some(next_path(&mut rest, "--summary")?),
            "-d" | "--data" => out.data = Some(next_path(&mut rest, "--data")?),
            "-n" | "--iterations" => {
                out.iterations = Some(number(rest.next(), "run: --iterations")?)
            }
            "--delay" => out.delay_ms = number(rest.next(), "run: --delay")?,
            "--retries" => out.retries = number(rest.next(), "run: --retries")?,
            "--update-snapshots" => out.update_snapshots = true,
            other if !other.starts_with('-') && out.target.is_empty() => {
                out.target = other.replace('\\', "/")
            }
            other => return Err(format!("run: unexpected {other}")),
        }
    }
    Ok(out)
}

fn run_run(args: &[String]) -> ExitCode {
    let options = match parse_run(args) {
        Ok(options) => options,
        Err(message) => return usage_error(&message),
    };
    let dir = &options.dir;
    let workspace = match collection::load_workspace(dir) {
        Ok(ws) => ws,
        Err(e) => return fail(e),
    };
    let environment_id = match &options.env_name {
        Some(name) => match find_environment(dir, name, &Keychain) {
            Ok(env) => Some(env.id),
            Err(e) => return fail(e),
        },
        None => None,
    };
    let requests = run::requests_under(&workspace.tree, &options.target);
    if requests.is_empty() {
        return fail(format!("nothing to run at '{}'", options.target));
    }
    let rows = match &options.data {
        Some(path) => match data::load_data(path) {
            Ok(rows) if rows.is_empty() => return fail(format!("{}: no rows", path.display())),
            Ok(rows) => rows,
            Err(e) => return fail(e),
        },
        None => Vec::new(),
    };
    // As in Postman: one iteration per data row unless told otherwise; past the last row,
    // the last row repeats.
    let iterations = options.iterations.unwrap_or(rows.len()).max(1);
    let mut session = match run::Session::open(dir, environment_id.as_deref(), &Keychain, true) {
        Ok(session) => session,
        Err(e) => return fail(e),
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => return fail(format!("could not start the runtime: {e}")),
    };

    let mut reports = Vec::new();
    let mut first = true;
    'run: for iteration in 0..iterations {
        if iterations > 1 {
            println!("\nIteration {}/{iterations}", iteration + 1);
        }
        let run_options = run::RunOptions {
            iteration,
            iterations,
            data: rows
                .get(iteration.min(rows.len().saturating_sub(1)))
                .cloned()
                .unwrap_or_default(),
            retries: options.retries,
            update_snapshots: options.update_snapshots,
        };
        for entry in &requests {
            if !first && options.delay_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(options.delay_ms));
            }
            first = false;
            let report = runtime.block_on(run::run_in(dir, entry, &mut session, &run_options));
            print_report(&report);
            let failed = !report.passed();
            reports.push(report);
            if failed && options.bail {
                break 'run;
            }
        }
    }
    let t = report::totals(&reports);
    println!(
        "\n{} requests: {} passed, {} failed · tests: {} passed, {} failed",
        t.requests, t.passed, t.failed, t.tests_passed, t.tests_failed
    );
    let outputs = [
        (
            &options.junit,
            "JUnit report",
            report::junit_xml(&workspace.name, &reports),
        ),
        (
            &options.html,
            "HTML report",
            report::html_report(&workspace.name, &reports, &report::now()),
        ),
        (
            &options.summary,
            "Summary",
            report::markdown_summary(&workspace.name, &reports),
        ),
    ];
    for (path, what, content) in outputs {
        if let Some(path) = path {
            if let Err(e) = std::fs::write(path, content) {
                return fail(format!("{}: {e}", path.display()));
            }
            println!("{what} written to {}", path.display());
        }
    }
    if t.failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn run_load(args: &[String]) -> ExitCode {
    let mut target = String::new();
    let mut dir = PathBuf::from(".");
    let mut env_name: Option<String> = None;
    let mut options = load::LoadOptions {
        users: 10,
        duration_secs: 30,
        ramp_up_secs: 0,
        data: Vec::new(),
    };
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let parsed: Result<(), String> = match arg.as_str() {
            "-w" | "--workspace" => rest
                .next()
                .map(|d| dir = PathBuf::from(d))
                .ok_or_else(|| "load: -w needs a DIR".into()),
            "-e" | "--env" => rest
                .next()
                .map(|e| env_name = Some(e.clone()))
                .ok_or_else(|| "load: -e needs an ENV".into()),
            "-u" | "--users" => number(rest.next(), "load: --users").map(|n| options.users = n),
            "-d" | "--duration" => rest
                .next()
                .and_then(|d| parse_seconds(d))
                .map(|s| options.duration_secs = s)
                .ok_or_else(|| "load: --duration needs seconds (30, 30s or 2m)".into()),
            "--data" => match rest.next().map(|p| data::load_data(Path::new(p))) {
                Some(Ok(rows)) => {
                    options.data = rows;
                    Ok(())
                }
                Some(Err(e)) => return fail(e),
                None => Err("load: --data needs a FILE".into()),
            },
            "--ramp-up" => rest
                .next()
                .and_then(|d| parse_seconds(d))
                .map(|s| options.ramp_up_secs = s)
                .ok_or_else(|| "load: --ramp-up needs seconds".into()),
            other if !other.starts_with('-') && target.is_empty() => {
                target = other.replace('\\', "/");
                Ok(())
            }
            other => Err(format!("load: unexpected {other}")),
        };
        if let Err(message) = parsed {
            return usage_error(&message);
        }
    }
    if options.users == 0 || options.users > load::MAX_USERS {
        return usage_error(&format!("load: --users goes from 1 to {}", load::MAX_USERS));
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
    let session = match run::Session::open(&dir, environment_id.as_deref(), &Keychain, false) {
        Ok(session) => session,
        Err(e) => return fail(e),
    };
    println!(
        "Load test: {} users, {} s, {} request(s) each loop",
        options.users,
        options.duration_secs,
        requests.len()
    );
    let last = std::sync::Mutex::new(u64::MAX);
    let summary = load::run_load(
        &dir,
        &requests,
        &session,
        &options,
        Default::default(),
        &|p: load::LoadProgress| {
            let mut last = last.lock().unwrap();
            if *last != p.elapsed_secs {
                *last = p.elapsed_secs;
                eprint!(
                    "\r  {:>3} s · {} done · {} failed   ",
                    p.elapsed_secs, p.done, p.failed
                );
            }
        },
    );
    eprintln!();
    let summary = match summary {
        Ok(summary) => summary,
        Err(e) => return fail(e),
    };
    print_load(&summary);
    if summary.total.failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn print_load(summary: &load::LoadSummary) {
    println!(
        "\n{:<32} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>8}",
        "Request", "count", "failed", "avg", "p50", "p90", "p95", "p99", "req/s"
    );
    for s in summary
        .requests
        .iter()
        .chain(std::iter::once(&summary.total))
    {
        let name: String = s.name.chars().take(32).collect();
        println!(
            "{name:<32} {:>7} {:>7} {:>5}ms {:>5}ms {:>5}ms {:>5}ms {:>5}ms {:>8}",
            s.count, s.failed, s.avg_ms, s.p50_ms, s.p90_ms, s.p95_ms, s.p99_ms, s.per_second
        );
    }
    for error in &summary.errors {
        println!("  ! {error}");
    }
}

fn print_report(report: &RunReport) {
    let mark = if report.passed() { "✓" } else { "✗" };
    match (&report.response, &report.error) {
        (Some(r), _) => println!(
            "{mark} {}  {} {} · {} ms{}",
            report.name,
            r.status,
            r.status_text,
            r.duration_ms,
            if report.attempts > 1 {
                format!(" · {} attempts", report.attempts)
            } else {
                String::new()
            }
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
    fn parses_run_options() {
        let options = parse_run(&args(&[
            "orders",
            "-d",
            "users.csv",
            "-n",
            "3",
            "--delay",
            "50",
            "--retries",
            "2",
            "--html",
            "r.html",
            "--update-snapshots",
        ]))
        .unwrap();
        assert_eq!(options.target, "orders");
        assert_eq!(options.data, Some(PathBuf::from("users.csv")));
        assert_eq!(
            (options.iterations, options.delay_ms, options.retries),
            (Some(3), 50, 2)
        );
        assert!(options.update_snapshots);
        assert!(parse_run(&args(&["--retries", "x"])).is_err());
        assert_eq!(parse_seconds("2m"), Some(120));
        assert_eq!(parse_seconds("45s"), Some(45));
        assert_eq!(parse_seconds("45"), Some(45));
        assert_eq!(parse_seconds("x"), None);
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

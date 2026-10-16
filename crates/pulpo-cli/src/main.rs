use pulpo_core::collection::{self, TreeNode};
use pulpo_core::environment::{self, Environment, SecretStore};
use pulpo_core::http::{self, Header, HttpRequest};
use pulpo_core::import;
use pulpo_core::keychain::Keychain;
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
More commands (run, scan) are coming. See https://github.com/pulpo-api/pulpo";

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

use pulpo_core::collection::{self, TreeNode};
use pulpo_core::http::{self, Header, HttpRequest};
use pulpo_core::{HttpMethod, Protocol};
use std::env;
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
    send <METHOD> <URL> [-H 'Name: value']... [-d BODY] [-i]
               Send one HTTP request and print the response body
               (-i also prints the status line and headers)
    version    Print the version
    help       Print this help

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
            Ok((request, include_headers)) => run_send(&request, include_headers),
            Err(message) => usage_error(&message),
        },
        Some("init") => run_init(&args[1..]),
        Some("ls") => run_ls(&args[1..]),
        Some("fmt") => run_fmt(&args[1..]),
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
            println!("{}", workspace.name);
            print_tree(&workspace.tree, "");
            ExitCode::SUCCESS
        }
        Err(e) => fail(e),
    }
}

fn print_tree(nodes: &[TreeNode], indent: &str) {
    for (i, node) in nodes.iter().enumerate() {
        let last = i + 1 == nodes.len();
        let branch = if last { "└── " } else { "├── " };
        match node {
            TreeNode::Folder { name, children, .. } => {
                println!("{indent}{branch}{name}/");
                let next = format!("{indent}{}", if last { "    " } else { "│   " });
                print_tree(children, &next);
            }
            TreeNode::Request(entry) => {
                let label = if entry.protocol == Protocol::Http {
                    entry.request.method.to_string()
                } else {
                    entry.protocol.as_str().to_uppercase()
                };
                println!(
                    "{indent}{branch}{label:<7} {}  {}",
                    entry.name, entry.request.url
                );
            }
        }
    }
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

fn usage_error(message: &str) -> ExitCode {
    eprintln!("{message}\n\n{HELP}");
    ExitCode::from(2)
}

fn parse_send(args: &[String]) -> Result<(HttpRequest, bool), String> {
    let method: HttpMethod = args
        .first()
        .ok_or("send: missing METHOD")?
        .parse()
        .map_err(|e| format!("send: {e}"))?;
    let url = args.get(1).ok_or("send: missing URL")?;
    let mut request = HttpRequest::new(method, url.clone());
    let mut include_headers = false;
    let mut rest = args[2..].iter();
    while let Some(flag) = rest.next() {
        match flag.as_str() {
            "-H" | "--header" => {
                let raw = rest.next().ok_or("send: -H needs 'Name: value'")?;
                let (name, value) = raw
                    .split_once(':')
                    .ok_or_else(|| format!("send: header must be 'Name: value', got '{raw}'"))?;
                request.headers.push(Header::new(name.trim(), value.trim()));
            }
            "-d" | "--data" => {
                request.body = Some(rest.next().ok_or("send: -d needs a body")?.clone());
            }
            "-i" | "--include" => include_headers = true,
            other => return Err(format!("send: unknown option {other}")),
        }
    }
    Ok((request, include_headers))
}

fn run_send(request: &HttpRequest, include_headers: bool) -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("could not start the runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(http::send(request)) {
        Ok(response) => {
            if include_headers {
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
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
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
        let (request, include) = parse_send(&args(&[
            "post",
            "http://localhost/orders",
            "-H",
            "X-Tenant-ID: test",
            "-d",
            "{}",
            "-i",
        ]))
        .unwrap();
        assert_eq!(request.method, HttpMethod::Post);
        assert_eq!(request.url, "http://localhost/orders");
        assert_eq!(request.headers, vec![Header::new("X-Tenant-ID", "test")]);
        assert_eq!(request.body.as_deref(), Some("{}"));
        assert!(include);
    }

    #[test]
    fn rejects_headers_without_colon() {
        assert!(parse_send(&args(&["GET", "http://x", "-H", "broken"])).is_err());
    }

    #[test]
    fn rejects_unknown_methods() {
        assert!(parse_send(&args(&["FETCH", "http://x"])).is_err());
    }
}

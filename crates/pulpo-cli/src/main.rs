use pulpo_core::http::{self, Header, HttpRequest};
use pulpo_core::HttpMethod;
use std::env;
use std::process::ExitCode;

const HELP: &str = "\
pulpo - run Pulpo collections from the terminal

USAGE:
    pulpo <COMMAND>

COMMANDS:
    send <METHOD> <URL> [-H 'Name: value']... [-d BODY] [-i]
               Send one HTTP request and print the response body
               (-i also prints the status line and headers)
    version    Print the version
    help       Print this help

More commands (run, fmt, scan) are coming. See https://github.com/pulpo-api/pulpo";

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
        Some(other) => usage_error(&format!("unknown command: {other}")),
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

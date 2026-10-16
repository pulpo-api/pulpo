//! Core engine of Pulpo.
//!
//! This crate holds the collection model shared by the desktop app and the CLI.
//! Protocol engines, scripting and code scanning will live in their own modules.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

pub mod certificates;
pub mod codegen;
pub mod collection;
pub mod cookies;
pub mod environment;
pub mod http;
pub mod import;
#[cfg(feature = "keychain")]
pub mod keychain;
pub mod local;
pub mod organize;
pub mod run;
pub mod script;
pub mod variables;

/// Version of the Pulpo core engine.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Protocols Pulpo is designed to speak.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Http,
    Graphql,
    Grpc,
    Websocket,
    Soap,
    Sse,
}

impl Protocol {
    /// All protocols, in the order shown in the UI.
    pub const ALL: [Protocol; 6] = [
        Protocol::Http,
        Protocol::Graphql,
        Protocol::Grpc,
        Protocol::Websocket,
        Protocol::Soap,
        Protocol::Sse,
    ];

    /// Short label used in the UI and in files.
    pub fn as_str(self) -> &'static str {
        match self {
            Protocol::Http => "http",
            Protocol::Graphql => "graphql",
            Protocol::Grpc => "grpc",
            Protocol::Websocket => "websocket",
            Protocol::Soap => "soap",
            Protocol::Sse => "sse",
        }
    }
}

/// HTTP methods supported by HTTP-based requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Head,
    Options,
}

impl fmt::Display for HttpMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            HttpMethod::Get => "GET",
            HttpMethod::Post => "POST",
            HttpMethod::Put => "PUT",
            HttpMethod::Patch => "PATCH",
            HttpMethod::Delete => "DELETE",
            HttpMethod::Head => "HEAD",
            HttpMethod::Options => "OPTIONS",
        };
        f.write_str(name)
    }
}

/// Error returned when a method name is not recognised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownMethod(pub String);

impl fmt::Display for UnknownMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown HTTP method: {}", self.0)
    }
}

impl std::error::Error for UnknownMethod {}

impl FromStr for HttpMethod {
    type Err = UnknownMethod;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_uppercase().as_str() {
            "GET" => Ok(HttpMethod::Get),
            "POST" => Ok(HttpMethod::Post),
            "PUT" => Ok(HttpMethod::Put),
            "PATCH" => Ok(HttpMethod::Patch),
            "DELETE" => Ok(HttpMethod::Delete),
            "HEAD" => Ok(HttpMethod::Head),
            "OPTIONS" => Ok(HttpMethod::Options),
            _ => Err(UnknownMethod(s.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_methods_case_insensitively() {
        assert_eq!("post".parse::<HttpMethod>(), Ok(HttpMethod::Post));
        assert_eq!("Delete".parse::<HttpMethod>(), Ok(HttpMethod::Delete));
    }

    #[test]
    fn rejects_unknown_methods() {
        assert!("FETCH".parse::<HttpMethod>().is_err());
    }

    #[test]
    fn displays_methods_in_uppercase() {
        assert_eq!(HttpMethod::Patch.to_string(), "PATCH");
    }

    #[test]
    fn lists_every_protocol_once() {
        let labels: Vec<_> = Protocol::ALL.iter().map(|p| p.as_str()).collect();
        assert_eq!(
            labels,
            ["http", "graphql", "grpc", "websocket", "soap", "sse"]
        );
    }
}

//! Client certificates (mTLS), per host, in `.pulpo/certificates.json`.
//!
//! They are local to this machine, like in Postman: private keys and their paths never
//! belong in git. A PFX/P12 password is written as a `{{variable}}`, so it can be a
//! secret of the environment (stored in the OS keychain).

use crate::collection::CollectionError;
use crate::import::base64;
use crate::local::{ensure_local_dir, LOCAL_DIR};
use p12_keystore::{KeyStore, Pkcs12ImportPolicy};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

const CERTIFICATES_FILE: &str = "certificates.json";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientCertificate {
    /// `api.bank.test`, `api.bank.test:8443` or `*.bank.test` (subdomains only).
    pub host: String,
    /// PEM certificate (or chain). With `key`, or alone if the file also holds the key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert: Option<String>,
    /// PEM private key, not encrypted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// PKCS#12 file (`.pfx`, `.p12`): certificate and key in one, with a password.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pfx: Option<String>,
    /// PFX password, usually `{{certPassword}}` (a secret variable of the environment).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passphrase: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CertificatesFile {
    version: u32,
    certificates: Vec<ClientCertificate>,
}

fn file_path(root: &Path) -> PathBuf {
    root.join(LOCAL_DIR).join(CERTIFICATES_FILE)
}

pub fn load_certificates(root: &Path) -> Vec<ClientCertificate> {
    fs::read_to_string(file_path(root))
        .ok()
        .and_then(|text| serde_json::from_str::<CertificatesFile>(&text).ok())
        .map(|file| file.certificates)
        .unwrap_or_default()
}

pub fn save_certificates(
    root: &Path,
    certificates: &[ClientCertificate],
) -> Result<(), CollectionError> {
    ensure_local_dir(root)?;
    let path = file_path(root);
    let file = CertificatesFile {
        version: 1,
        certificates: certificates.to_vec(),
    };
    let text =
        serde_json::to_string_pretty(&file).map_err(|e| CollectionError::Io(e.to_string()))?;
    fs::write(&path, text + "\n")
        .map_err(|e| CollectionError::Io(format!("{}: {e}", path.display())))
}

/// The certificate for a URL: an exact host (with port, if the entry has one) wins over
/// a `*.domain` entry.
pub fn find<'a>(certificates: &'a [ClientCertificate], url: &Url) -> Option<&'a ClientCertificate> {
    let host = url.host_str()?.to_ascii_lowercase();
    let port = url.port_or_known_default();
    let matches = |entry: &ClientCertificate, wildcard: bool| {
        let pattern = entry.host.trim().to_ascii_lowercase();
        let (name, entry_port) = match pattern.rsplit_once(':') {
            Some((name, p)) if p.chars().all(|c| c.is_ascii_digit()) => {
                (name.to_string(), p.parse::<u16>().ok())
            }
            _ => (pattern.clone(), None),
        };
        if entry_port.is_some() && entry_port != port {
            return false;
        }
        match name.strip_prefix("*.") {
            Some(domain) => wildcard && host.ends_with(&format!(".{domain}")),
            None => !wildcard && host == name,
        }
    };
    certificates
        .iter()
        .find(|c| matches(c, false))
        .or_else(|| certificates.iter().find(|c| matches(c, true)))
}

/// A path from the certificates file: `~/...` is the home folder, relative paths start at
/// the workspace, absolute paths are kept.
fn local_path(root: &Path, path: &str) -> PathBuf {
    let path = path.trim();
    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            return PathBuf::from(home).join(rest);
        }
    }
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn read(root: &Path, path: &str) -> Result<Vec<u8>, String> {
    let full = local_path(root, path);
    fs::read(&full).map_err(|e| format!("{}: {e}", full.display()))
}

fn pem_block(label: &str, der: &[u8]) -> String {
    let encoded = base64(der);
    let mut out = format!("-----BEGIN {label}-----\n");
    for line in encoded.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(line).unwrap_or_default());
        out.push('\n');
    }
    out.push_str(&format!("-----END {label}-----\n"));
    out
}

/// The certificate and key as one PEM, ready for the TLS client. `passphrase` must
/// already have its variables resolved.
pub fn identity_pem(
    root: &Path,
    certificate: &ClientCertificate,
    passphrase: &str,
) -> Result<Vec<u8>, String> {
    if let Some(pfx) = certificate.pfx.as_deref().filter(|p| !p.trim().is_empty()) {
        let data = read(root, pfx)?;
        let store = KeyStore::from_pkcs12(&data, passphrase, Pkcs12ImportPolicy::Relaxed)
            .map_err(|e| format!("{pfx}: cannot open it (wrong password?): {e}"))?;
        let (_, chain) = store
            .private_key_chain()
            .ok_or_else(|| format!("{pfx}: no private key inside"))?;
        let mut pem = String::new();
        for cert in chain.certs() {
            pem.push_str(&pem_block("CERTIFICATE", cert.as_der()));
        }
        pem.push_str(&pem_block("PRIVATE KEY", chain.key().as_der()));
        return Ok(pem.into_bytes());
    }
    let cert = certificate
        .cert
        .as_deref()
        .filter(|c| !c.trim().is_empty())
        .ok_or_else(|| format!("{}: set a PEM certificate or a PFX file", certificate.host))?;
    let mut pem = read(root, cert)?;
    if let Some(key) = certificate.key.as_deref().filter(|k| !k.trim().is_empty()) {
        pem.push(b'\n');
        pem.extend(read(root, key)?);
    }
    let text = String::from_utf8_lossy(&pem);
    if text.contains("ENCRYPTED PRIVATE KEY") || text.contains("Proc-Type: 4,ENCRYPTED") {
        return Err(format!(
            "{}: encrypted PEM keys are not supported; use the .pfx/.p12 file or an unencrypted key",
            certificate.host
        ));
    }
    if !text.contains("PRIVATE KEY-----") {
        return Err(format!(
            "{}: no private key found (set the key file)",
            certificate.host
        ));
    }
    Ok(pem)
}

#[cfg(test)]
mod tests {
    use super::*;
    use p12_keystore::{Certificate, KeyStoreEntry, PrivateKey, PrivateKeyChain};

    fn cert(host: &str) -> ClientCertificate {
        ClientCertificate {
            host: host.into(),
            cert: Some(format!("{host}.pem")),
            ..Default::default()
        }
    }

    #[test]
    fn finds_the_certificate_of_a_host() {
        let all = vec![
            cert("*.bank.test"),
            cert("api.bank.test:8443"),
            cert("www.bank.test"),
        ];
        let url = |u: &str| Url::parse(u).unwrap();
        let host_of = |u: &str| find(&all, &url(u)).map(|c| c.host.clone());
        assert_eq!(
            host_of("https://www.bank.test/x"),
            Some("www.bank.test".into())
        );
        assert_eq!(
            host_of("https://api.bank.test:8443/"),
            Some("api.bank.test:8443".into())
        );
        assert_eq!(
            host_of("https://api.bank.test/"),
            Some("*.bank.test".into()),
            "other port: the wildcard"
        );
        assert_eq!(
            host_of("https://bank.test/"),
            None,
            "a wildcard is for subdomains"
        );
        assert_eq!(host_of("https://other.test/"), None);
    }

    #[test]
    fn builds_identities_from_pem_and_pfx() {
        let dir = tempfile::tempdir().unwrap();
        let generated = rcgen::generate_simple_self_signed(vec!["client.test".into()]).unwrap();
        fs::write(dir.path().join("client.crt"), generated.cert.pem()).unwrap();
        fs::write(
            dir.path().join("client.key"),
            generated.key_pair.serialize_pem(),
        )
        .unwrap();

        let pem = ClientCertificate {
            host: "api.test".into(),
            cert: Some("client.crt".into()),
            key: Some("client.key".into()),
            ..Default::default()
        };
        let identity = identity_pem(dir.path(), &pem, "").unwrap();
        assert!(reqwest::Identity::from_pem(&identity).is_ok());

        let mut store = KeyStore::new();
        store.add_entry(
            "client",
            KeyStoreEntry::PrivateKeyChain(PrivateKeyChain::new(
                vec![1u8, 2, 3],
                PrivateKey::from_der(&generated.key_pair.serialize_der()).unwrap(),
                vec![Certificate::from_der(generated.cert.der()).unwrap()],
            )),
        );
        fs::write(
            dir.path().join("client.pfx"),
            store.writer("s3cret").write().unwrap(),
        )
        .unwrap();
        let pfx = ClientCertificate {
            host: "api.test".into(),
            pfx: Some("client.pfx".into()),
            passphrase: Some("{{certPassword}}".into()),
            ..Default::default()
        };
        let identity = identity_pem(dir.path(), &pfx, "s3cret").unwrap();
        assert!(reqwest::Identity::from_pem(&identity).is_ok());
        let wrong = identity_pem(dir.path(), &pfx, "nope").unwrap_err();
        assert!(wrong.contains("wrong password"), "{wrong}");

        let missing_key = ClientCertificate {
            key: None,
            ..pem.clone()
        };
        assert!(identity_pem(dir.path(), &missing_key, "")
            .unwrap_err()
            .contains("no private key"));
    }

    #[test]
    fn keeps_certificates_in_pulpo_dir() {
        let ws = tempfile::tempdir().unwrap();
        let all = vec![cert("api.bank.test")];
        save_certificates(ws.path(), &all).unwrap();
        assert_eq!(load_certificates(ws.path()), all);
        assert!(ws.path().join(".pulpo/.gitignore").exists());
    }
}

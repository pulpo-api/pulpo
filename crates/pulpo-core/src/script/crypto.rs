//! Digests behind the scripts' `CryptoJS` object. Inputs and outputs are hex strings.

use hmac::{Hmac, Mac};
use md5::Md5;
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};

fn from_hex(hex: &str) -> Result<Vec<u8>, String> {
    if hex.len() % 2 != 0 {
        return Err("odd hex length".into());
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hmac<M: Mac + hmac::digest::KeyInit>(key: &[u8], data: &[u8]) -> Result<Vec<u8>, String> {
    let mut mac = <M as Mac>::new_from_slice(key).map_err(|e| e.to_string())?;
    mac.update(data);
    Ok(mac.finalize().into_bytes().to_vec())
}

/// `algorithm` is one of md5, sha1, sha256, sha512, hmac-md5, hmac-sha1, hmac-sha256, hmac-sha512.
pub fn digest(algorithm: &str, key_hex: &str, data_hex: &str) -> Result<String, String> {
    let data = from_hex(data_hex)?;
    let key = from_hex(key_hex)?;
    let bytes = match algorithm {
        "md5" => Md5::digest(&data).to_vec(),
        "sha1" => Sha1::digest(&data).to_vec(),
        "sha256" => Sha256::digest(&data).to_vec(),
        "sha512" => Sha512::digest(&data).to_vec(),
        "hmac-md5" => hmac::<Hmac<Md5>>(&key, &data)?,
        "hmac-sha1" => hmac::<Hmac<Sha1>>(&key, &data)?,
        "hmac-sha256" => hmac::<Hmac<Sha256>>(&key, &data)?,
        "hmac-sha512" => hmac::<Hmac<Sha512>>(&key, &data)?,
        other => return Err(format!("unsupported algorithm {other}")),
    };
    Ok(to_hex(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_known_values() {
        assert_eq!(
            digest("sha256", "", "616263").unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            digest("md5", "", "").unwrap(),
            "d41d8cd98f00b204e9800998ecf8427e"
        );
        assert_eq!(digest("hmac-sha256", "6b6579", "").unwrap().len(), 64);
        assert!(digest("crc32", "", "").is_err());
        assert!(digest("sha256", "", "abc").is_err());
    }
}

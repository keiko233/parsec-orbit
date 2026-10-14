//! What makes a download trustworthy: sha256 digests and ed25519 signatures.
//!
//! The index lists the sha256 of every package; the index itself is signed.
//! A client therefore trusts a package if its digest matches a signed index
//! entry, and trusts the index if one of the keys it was configured with
//! signed it. Nothing is trusted because of where it came from.
//!
//! Keys and signatures travel as base64 text. The key list is
//! `trusted-keys.toml`, one `[[key]]` table per key (see
//! [`parse_trusted_keys_toml`]). The older `trusted-keys.txt` format, one
//! `ed25519 <base64 public key> [comment]` per line, is still read and is
//! converted once by [`load_trusted_keys`].

use std::path::Path;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::source::write_atomic;
use crate::Error;

/// The key list in the Orbit root.
pub const KEYS_TOML: &str = "trusted-keys.toml";
/// The line-based key list that `trusted-keys.toml` replaces.
pub const KEYS_TXT: &str = "trusted-keys.txt";

/// Lowercase hex sha256.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A new key pair: `(secret, public)`, both base64.
pub fn generate_keypair() -> Result<(String, String), Error> {
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|error| Error::Io(format!("no randomness: {error}")))?;
    let key = SigningKey::from_bytes(&seed);
    Ok((
        STANDARD.encode(seed),
        STANDARD.encode(key.verifying_key().to_bytes()),
    ))
}

fn bytes32(text: &str, what: &str) -> Result<[u8; 32], Error> {
    let bytes = STANDARD
        .decode(text.trim())
        .map_err(|error| Error::Signature(format!("{what} is not base64: {error}")))?;
    bytes
        .try_into()
        .map_err(|_| Error::Signature(format!("{what} is not 32 bytes")))
}

pub fn signing_key(secret_base64: &str) -> Result<SigningKey, Error> {
    Ok(SigningKey::from_bytes(&bytes32(
        secret_base64,
        "the secret key",
    )?))
}

pub fn public_key(base64: &str) -> Result<VerifyingKey, Error> {
    VerifyingKey::from_bytes(&bytes32(base64, "the public key")?)
        .map_err(|error| Error::Signature(format!("the public key is not valid: {error}")))
}

/// Signs `message`; the signature as base64.
pub fn sign(message: &[u8], key: &SigningKey) -> String {
    STANDARD.encode(key.sign(message).to_bytes())
}

/// One entry of a key list.
#[derive(Debug, Clone)]
pub struct TrustedKey {
    pub key: VerifyingKey,
    pub comment: String,
}

/// Reads a key list.
pub fn parse_trusted_keys(text: &str) -> Result<Vec<TrustedKey>, Error> {
    let mut keys = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(3, char::is_whitespace);
        let (kind, data) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        if kind != "ed25519" || data.is_empty() {
            return Err(Error::Signature(format!(
                "line {}: expected `ed25519 <base64 key> [comment]`",
                number + 1
            )));
        }
        keys.push(TrustedKey {
            key: public_key(data)?,
            comment: parts.next().unwrap_or("").trim().to_string(),
        });
    }
    Ok(keys)
}

/// `trusted-keys.toml`: one `[[key]]` table per key.
#[derive(Deserialize)]
struct KeyList {
    #[serde(default)]
    key: Vec<KeyTable>,
}

#[derive(Deserialize)]
struct KeyTable {
    algorithm: String,
    public: String,
    #[serde(default)]
    comment: String,
}

/// Reads a `trusted-keys.toml`. Unknown keys are ignored, as in the index.
pub fn parse_trusted_keys_toml(text: &str) -> Result<Vec<TrustedKey>, Error> {
    let list: KeyList =
        toml::from_str(text).map_err(|error| Error::Signature(format!("trusted keys: {error}")))?;
    list.key
        .into_iter()
        .enumerate()
        .map(|(number, table)| {
            if table.algorithm != "ed25519" {
                return Err(Error::Signature(format!(
                    "key {}: algorithm {:?} is not ed25519",
                    number + 1,
                    table.algorithm
                )));
            }
            Ok(TrustedKey {
                key: public_key(&table.public)?,
                comment: table.comment,
            })
        })
        .collect()
}

/// The `[[key]]` table for `key`, ready to append to a `trusted-keys.toml`.
pub fn to_toml_block(key: &TrustedKey) -> String {
    let mut block = format!(
        "[[key]]\nalgorithm = \"ed25519\"\npublic = \"{}\"\n",
        STANDARD.encode(key.key.to_bytes())
    );
    if !key.comment.is_empty() {
        block.push_str(&format!("comment = {}\n", toml_string(&key.comment)));
    }
    block
}

/// `text` as a TOML basic string, quotes included.
fn toml_string(text: &str) -> String {
    let mut quoted = String::from("\"");
    for character in text.chars() {
        match character {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            control if control.is_control() => {
                quoted.push_str(&format!("\\u{:04X}", control as u32))
            }
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    quoted
}

/// The keys in the key list of the Orbit root `dir`: `trusted-keys.toml`, or
/// the older `trusted-keys.txt` while there is no TOML file. A text list is
/// converted once, by writing the TOML file beside it; the text file is left
/// as it was and is not read again. No list at all is an empty list.
pub fn load_trusted_keys(dir: &Path) -> Result<Vec<TrustedKey>, Error> {
    let toml_path = dir.join(KEYS_TOML);
    if let Some(text) = read_optional(&toml_path)? {
        return parse_trusted_keys_toml(&text);
    }
    let Some(text) = read_optional(&dir.join(KEYS_TXT))? else {
        return Ok(Vec::new());
    };
    let keys = parse_trusted_keys(&text)?;
    let mut converted = format!("# Trusted keys, converted from {KEYS_TXT}.\n");
    for key in &keys {
        converted.push('\n');
        converted.push_str(&to_toml_block(key));
    }
    write_atomic(&toml_path, converted.as_bytes())?;
    Ok(keys)
}

fn read_optional(path: &Path) -> Result<Option<String>, Error> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Error::Io(format!("{}: {error}", path.display()))),
    }
}

/// Checks that one of `trusted` signed `message`; returns which.
pub fn verify(
    message: &[u8],
    signature_base64: &str,
    trusted: &[TrustedKey],
) -> Result<usize, Error> {
    if trusted.is_empty() {
        return Err(Error::Signature(
            "no trusted keys are configured, so no signature can be accepted".to_string(),
        ));
    }
    let bytes = STANDARD
        .decode(signature_base64.trim())
        .map_err(|error| Error::Signature(format!("the signature is not base64: {error}")))?;
    let signature = Signature::from_slice(&bytes)
        .map_err(|_| Error::Signature("the signature has the wrong length".to_string()))?;
    trusted
        .iter()
        .position(|trusted| trusted.key.verify_strict(message, &signature).is_ok())
        .ok_or_else(|| Error::Signature("no trusted key signed this index".to_string()))
}

/// Checks a package against the digest the index promised.
pub fn check_digest(bytes: &[u8], expected_hex: &str) -> Result<(), Error> {
    let actual = sha256_hex(bytes);
    if actual == expected_hex {
        Ok(())
    } else {
        Err(Error::Hash(format!(
            "the package has sha256 {actual}, the index says {expected_hex}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_the_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    fn keys() -> (SigningKey, Vec<TrustedKey>, String) {
        let (secret, public) = generate_keypair().unwrap();
        let key = signing_key(&secret).unwrap();
        let list = format!("# official\ned25519 {public} the maintainers\n");
        (key, parse_trusted_keys(&list).unwrap(), public)
    }

    #[test]
    fn a_signature_verifies_only_for_the_signed_bytes() {
        let (key, trusted, _) = keys();
        let signature = sign(b"the index", &key);
        assert_eq!(verify(b"the index", &signature, &trusted).unwrap(), 0);
        assert!(verify(b"the index!", &signature, &trusted).is_err());
        assert!(verify(b"", &signature, &trusted).is_err());
    }

    #[test]
    fn a_signature_by_another_key_is_refused() {
        let (_, trusted, _) = keys();
        let (other, _, _) = keys();
        let signature = sign(b"the index", &other);
        let error = verify(b"the index", &signature, &trusted)
            .unwrap_err()
            .to_string();
        assert!(error.contains("no trusted key"), "{error}");
    }

    #[test]
    fn any_listed_key_may_sign_and_the_comment_is_kept() {
        let (first, ..) = keys();
        let (second_secret, second_public) = generate_keypair().unwrap();
        let second = signing_key(&second_secret).unwrap();
        let list = format!(
            "ed25519 {}\ned25519 {second_public} backup key # trailing\n",
            STANDARD.encode(first.verifying_key().to_bytes())
        );
        let trusted = parse_trusted_keys(&list).unwrap();
        assert_eq!(trusted[1].comment, "backup key");
        assert_eq!(verify(b"m", &sign(b"m", &second), &trusted).unwrap(), 1);
        assert_eq!(verify(b"m", &sign(b"m", &first), &trusted).unwrap(), 0);
    }

    #[test]
    fn nothing_is_accepted_without_trusted_keys() {
        let (key, ..) = keys();
        let error = verify(b"m", &sign(b"m", &key), &[])
            .unwrap_err()
            .to_string();
        assert!(error.contains("no trusted keys"), "{error}");
    }

    #[test]
    fn malformed_keys_and_signatures_are_refused() {
        assert!(parse_trusted_keys("rsa AAAA").is_err());
        assert!(parse_trusted_keys("ed25519").is_err());
        assert!(parse_trusted_keys("ed25519 not-base64!").is_err());
        assert!(parse_trusted_keys(&format!("ed25519 {}", STANDARD.encode([1u8; 7]))).is_err());
        assert!(parse_trusted_keys("# nothing here\n\n").unwrap().is_empty());

        let (_, trusted, _) = keys();
        assert!(verify(b"m", "not base64!", &trusted).is_err());
        assert!(verify(b"m", &STANDARD.encode([0u8; 10]), &trusted).is_err());
        assert!(signing_key("short").is_err());
        assert!(public_key(&STANDARD.encode([0u8; 31])).is_err());
    }

    struct Dir(std::path::PathBuf);
    impl Dir {
        fn new(label: &str) -> Dir {
            use std::sync::atomic::{AtomicU32, Ordering};
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let path = std::env::temp_dir().join(format!(
                "parsec-orbit-trust-{label}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Dir(path)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn public_base64() -> String {
        generate_keypair().unwrap().1
    }

    #[test]
    fn a_toml_key_block_round_trips_with_awkward_comments() {
        let key = TrustedKey {
            key: public_key(&public_base64()).unwrap(),
            comment: "the \"maintainers\" \\ backup\nsecond line".into(),
        };
        let text = format!("# header\n\n{}", to_toml_block(&key));
        let parsed = parse_trusted_keys_toml(&text).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].key, key.key);
        assert_eq!(parsed[0].comment, key.comment);
    }

    #[test]
    fn malformed_toml_key_lists_are_refused() {
        let public = public_base64();
        let good = format!("[[key]]\nalgorithm = \"ed25519\"\npublic = \"{public}\"\n");
        assert_eq!(parse_trusted_keys_toml(&good).unwrap().len(), 1);
        assert!(parse_trusted_keys_toml("[[key]]\nalgorithm = \"ed25519\"\n").is_err());
        assert!(parse_trusted_keys_toml("[[key]\n").is_err(), "not TOML");
        assert!(parse_trusted_keys_toml("key = 3\n").is_err(), "wrong type");
        let rsa = parse_trusted_keys_toml(&good.replace("ed25519", "rsa")).unwrap_err();
        assert!(rsa.to_string().contains("not ed25519"), "{rsa}");
        assert!(parse_trusted_keys_toml(
            "[[key]]\nalgorithm = \"ed25519\"\npublic = \"not-base64!\"\n"
        )
        .is_err());
        assert!(parse_trusted_keys_toml(&format!("{good}future = true\n")).is_ok());
        assert!(parse_trusted_keys_toml("# nothing here\n")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn the_text_key_list_is_converted_once_and_left_alone() {
        let dir = Dir::new("convert");
        let (first, second) = (public_base64(), public_base64());
        let legacy = format!("# old list\ned25519 {first} the maintainers\n\ned25519 {second}\n");
        std::fs::write(dir.0.join(KEYS_TXT), &legacy).unwrap();

        let keys = load_trusted_keys(&dir.0).unwrap();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].comment, "the maintainers");
        let converted = std::fs::read_to_string(dir.0.join(KEYS_TOML)).unwrap();
        let again = parse_trusted_keys_toml(&converted).unwrap();
        let bytes = |keys: &[TrustedKey]| keys.iter().map(|k| k.key.to_bytes()).collect::<Vec<_>>();
        assert_eq!(bytes(&again), bytes(&keys));
        assert_eq!(again[0].comment, "the maintainers");
        assert_eq!(
            std::fs::read_to_string(dir.0.join(KEYS_TXT)).unwrap(),
            legacy,
            "the text list is not rewritten"
        );

        // Once the TOML file exists the text list is not read again.
        std::fs::write(dir.0.join(KEYS_TXT), "ed25519 not-a-key\n").unwrap();
        assert_eq!(load_trusted_keys(&dir.0).unwrap().len(), 2);
    }

    #[test]
    fn no_key_list_is_empty_and_a_malformed_toml_list_is_not_replaced() {
        let dir = Dir::new("precedence");
        assert!(load_trusted_keys(&dir.0).unwrap().is_empty());
        assert!(!dir.0.join(KEYS_TOML).exists(), "nothing created");

        std::fs::write(
            dir.0.join(KEYS_TXT),
            format!("ed25519 {}\n", public_base64()),
        )
        .unwrap();
        std::fs::write(dir.0.join(KEYS_TOML), "[[key]\n").unwrap();
        assert!(
            load_trusted_keys(&dir.0).is_err(),
            "a broken TOML list is an error, not a fallback to the text list"
        );
    }

    #[test]
    fn the_digest_check_names_both_sides() {
        let digest = sha256_hex(b"package");
        check_digest(b"package", &digest).unwrap();
        let error = check_digest(b"tampered", &digest).unwrap_err().to_string();
        assert!(
            error.contains(&digest) && error.contains(&sha256_hex(b"tampered")),
            "{error}"
        );
    }
}

//! What makes a download trustworthy: sha256 digests and ed25519 signatures.
//!
//! The index lists the sha256 of every package; the index itself is signed.
//! A client therefore trusts a package if its digest matches a signed index
//! entry, and trusts the index if one of the keys it was configured with
//! signed it. Nothing is trusted because of where it came from.
//!
//! Keys and signatures travel as base64 text. A key list is a text file with
//! one `ed25519 <base64 public key> [comment]` per line; `#` starts a comment.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};

use crate::Error;

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

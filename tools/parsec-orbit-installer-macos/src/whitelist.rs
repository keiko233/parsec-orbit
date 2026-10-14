//! The supported vendor stubs.
//!
//! Only a stub whose complete SHA-256 is listed here is patched. The list
//! carries the arm64 Parsec 150.x bootstrap measured by the M1a disposable
//! acceptance runs (`docs/design/macos/bootstrap-checkpoint.md`); a vendor
//! update with a different stub refuses until its layout and hash are
//! measured and recorded here. The whitelist checks provenance of the
//! *input*; it does not replace the code-signature check of the selected
//! bundle.

/// Verified stub hashes, lowercase hex.
pub const SUPPORTED_STUBS: &[&str] = &[
    // Parsec 150.101a/104a arm64 bootstrap (M1a checkpoint, 2026-10-14).
    "785f985d7f0cf6cffda8966b776a61b6c6cb0c79e582a8e3f35cda36305be76d",
];

/// The accepted stub set. Tests build a fixture whitelist with their own
/// hashes through [`Whitelist::from_hashes`]; nothing else can widen the
/// shipped list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Whitelist {
    hashes: Vec<String>,
}

impl Whitelist {
    /// The hashes this build accepts.
    pub fn supported() -> Whitelist {
        Whitelist::from_hashes(SUPPORTED_STUBS.iter().map(|hash| hash.to_string()))
    }

    /// A whitelist with exactly the given hashes (lowercased).
    pub fn from_hashes<I: IntoIterator<Item = String>>(hashes: I) -> Whitelist {
        Whitelist {
            hashes: hashes
                .into_iter()
                .map(|hash| hash.to_ascii_lowercase())
                .collect(),
        }
    }

    pub fn hashes(&self) -> &[String] {
        &self.hashes
    }

    /// Whether the hex SHA-256 belongs to a supported stub.
    pub fn permits(&self, sha256: &str) -> bool {
        let sha256 = sha256.to_ascii_lowercase();
        self.hashes.contains(&sha256)
    }
}

impl Default for Whitelist {
    fn default() -> Self {
        Whitelist::supported()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_list_holds_the_measured_stub() {
        let whitelist = Whitelist::supported();
        assert!(whitelist.permits(SUPPORTED_STUBS[0]));
        assert!(whitelist.permits(&SUPPORTED_STUBS[0].to_ascii_uppercase()));
        assert!(!whitelist.permits(&"0".repeat(64)));
        assert_eq!(whitelist.hashes().len(), SUPPORTED_STUBS.len());
    }

    #[test]
    fn a_fixture_list_is_exact() {
        let whitelist = Whitelist::from_hashes(["AB".repeat(32)]);
        assert!(whitelist.permits(&"ab".repeat(32)));
        assert!(!whitelist.permits(SUPPORTED_STUBS[0]));
    }
}

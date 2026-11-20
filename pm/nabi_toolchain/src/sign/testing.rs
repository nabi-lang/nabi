//! Release sets signed with a throwaway key, for tests: built only with
//! the `test-sign` feature, never into a release.

use crate::sign::{blake2b::blake2b512, ed25519, encode64};

/// A throwaway key: its secret seed and key id.
pub struct TestKey {
    pub seed: [u8; 32],
    pub id: [u8; 8],
}

impl TestKey {
    /// The key's minisign public key line (for a `--key` file).
    pub fn public(&self) -> String {
        let (pk, _) = ed25519::sign(&self.seed, b"");
        let mut raw = b"Ed".to_vec();
        raw.extend_from_slice(&self.id);
        raw.extend_from_slice(&pk);
        encode64(&raw)
    }

    /// A prehashed (`ED`) minisign signature of `msg` with trusted
    /// comment `comment`, as `minisign -S` writes it.
    pub fn minisig(&self, msg: &[u8], comment: &str) -> String {
        let (_, sig) = ed25519::sign(&self.seed, &blake2b512(msg));
        let mut body = b"ED".to_vec();
        body.extend_from_slice(&self.id);
        body.extend_from_slice(&sig);
        let mut both = sig.to_vec();
        both.extend_from_slice(comment.as_bytes());
        let (_, global) = ed25519::sign(&self.seed, &both);
        format!(
            "untrusted comment: test signature\n{}\ntrusted comment: {comment}\n{}\n",
            encode64(&body),
            encode64(&global)
        )
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn verifies() {
        let k = super::TestKey {
            seed: [7; 32],
            id: *b"testkey!",
        };
        let pk = super::super::PublicKey::parse(&k.public()).unwrap();
        let sig = k.minisig(b"sums", "c");
        assert_eq!(super::super::verify(&pk, b"sums", &sig, "x").unwrap(), "c");
        assert!(super::super::verify(&pk, b"sumz", &sig, "x").is_err());
    }
}

//! Release signatures: minisign's format over Ed25519, verify only.
//!
//! A release carries `SHA256SUMS` (the SHA-256 of every archive) and
//! `SHA256SUMS.minisig`, signed with the release key (`keys/release.pub`,
//! built in). A download is trusted only when that signature verifies and
//! its archive's line in `SHA256SUMS` matches. `--key <file>` (a minisign
//! .pub file) on `nabi toolchain install` / `update` replaces the built-in
//! key, to test a release set signed with a throwaway key; it is a flag,
//! never the environment, and a warning names the key on every use.
//!
//! ```text
//! public key   base64("Ed" || key id (8, little endian) || key (32))
//! signature    untrusted comment: <text>
//!              base64(alg (2) || key id (8) || sig (64))
//!              trusted comment: <text>
//!              base64(global sig (64))
//! ```
//!
//! alg `Ed` signs the file itself, `ED` (minisign's default) its
//! BLAKE2b-512; the global signature covers sig || trusted comment.

pub mod blake2b;
pub mod ed25519;
pub mod sha512;
#[cfg(any(test, feature = "test-sign"))]
pub mod testing;

/// The release key, `keys/release.pub`.
pub const RELEASE_KEY: &str = include_str!("../../../keys/release.pub");

/// A minisign public key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicKey {
    pub id: [u8; 8],
    pub key: [u8; 32],
}

impl PublicKey {
    /// A key file's text (`untrusted comment:` line optional) or its
    /// base64 line alone.
    pub fn parse(text: &str) -> Result<Self, String> {
        let line = text
            .lines()
            .map(str::trim)
            .rfind(|l| !l.is_empty() && !l.starts_with("untrusted comment:"))
            .unwrap_or("");
        let raw = base64(line).filter(|b| b.len() == 42 && b[..2] == *b"Ed");
        let Some(raw) = raw else {
            return Err(format!(
                "`{line}` is not a minisign public key; give the base64 line of a \
                 minisign .pub file (it starts with `RW`)"
            ));
        };
        let mut id = [0; 8];
        let mut key = [0; 32];
        id.copy_from_slice(&raw[2..10]);
        key.copy_from_slice(&raw[10..]);
        Ok(Self { id, key })
    }

    /// The key id as minisign prints it.
    pub fn id_hex(&self) -> String {
        key_id(self.id)
    }
}

fn key_id(id: [u8; 8]) -> String {
    format!("{:016X}", u64::from_le_bytes(id))
}

/// The key downloads are checked against: the key in minisign public key
/// file `file` if given (with a warning naming it), else the release key.
pub fn trusted(file: Option<&std::path::Path>) -> Result<PublicKey, String> {
    let Some(file) = file else {
        return PublicKey::parse(RELEASE_KEY);
    };
    let text = std::fs::read_to_string(file).map_err(|e| {
        format!(
            "--key {}: {e}; give a minisign public key file, or leave out --key to use \
             the nabi release key",
            file.display()
        )
    })?;
    let key = PublicKey::parse(&text).map_err(|e| format!("--key {}: {e}", file.display()))?;
    eprintln!(
        "{} trusting key {} from {} instead of the nabi release key {}",
        nabic_core::term::label(
            nabic_core::term::Stream::Stderr,
            nabic_core::term::Role::Warning
        ),
        key.id_hex(),
        file.display(),
        PublicKey::parse(RELEASE_KEY)
            .map(|k| k.id_hex())
            .unwrap_or_default()
    );
    Ok(key)
}

/// Check `sig` (a .minisig file's text) is `pk`'s signature of `msg`,
/// trusted comment included; the trusted comment. `what` names the
/// signed file in errors.
pub fn verify(pk: &PublicKey, msg: &[u8], sig: &str, what: &str) -> Result<String, String> {
    let bad = |why: &str| {
        format!(
            "{what}.minisig {why}; the download is damaged or was altered, so nothing was \
             installed. Try again later, and report it at \
             https://github.com/nabi-lang/nabi/security if it persists"
        )
    };
    let lines: Vec<&str> = sig.lines().map(|l| l.trim_end_matches('\r')).collect();
    let [untrusted, body, trusted, global, rest @ ..] = lines.as_slice() else {
        return Err(bad("is not a minisign signature (it needs four lines)"));
    };
    if !untrusted.starts_with("untrusted comment:") || rest.iter().any(|l| !l.trim().is_empty()) {
        return Err(bad("is not a minisign signature"));
    }
    let Some(comment) = trusted.strip_prefix("trusted comment: ") else {
        return Err(bad("has no trusted comment line"));
    };
    let body = base64(body.trim()).filter(|b| b.len() == 74);
    let global = base64(global.trim()).filter(|b| b.len() == 64);
    let (Some(body), Some(global)) = (body, global) else {
        return Err(bad("is not a minisign signature (bad base64)"));
    };
    let mut id = [0; 8];
    id.copy_from_slice(&body[2..10]);
    if id != pk.id {
        return Err(bad(&format!(
            "is signed with key {}, not the release key {}",
            key_id(id),
            pk.id_hex()
        )));
    }
    let mut s = [0; 64];
    s.copy_from_slice(&body[10..]);
    let ok = match &body[..2] {
        b"Ed" => ed25519::verify(&pk.key, msg, &s),
        b"ED" => ed25519::verify(&pk.key, &blake2b::blake2b512(msg), &s),
        _ => return Err(bad("uses a signature algorithm other than Ed25519")),
    };
    if !ok {
        return Err(bad(&format!("does not verify against {what}")));
    }
    let mut g = [0; 64];
    g.copy_from_slice(&global);
    let mut signed = s.to_vec();
    signed.extend_from_slice(comment.as_bytes());
    if !ed25519::verify(&pk.key, &signed, &g) {
        return Err(bad("has a trusted comment that does not verify"));
    }
    Ok(comment.to_owned())
}

/// The digest `SHA256SUMS` lists for file `name` (`<hex>  <name>` lines,
/// `*<name>` for binary mode).
pub fn digest_of(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let (hex, file) = l.trim_end_matches('\r').split_once(char::is_whitespace)?;
        let file = file.trim_start();
        let file = file.strip_prefix('*').unwrap_or(file);
        (file == name && hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| hex.to_ascii_lowercase())
    })
}

/// Standard base64 with padding.
#[cfg(any(test, feature = "test-sign"))]
fn encode64(b: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in b.chunks(3) {
        let v = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                s.push(A[((v >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

/// Standard base64 with padding; `None` on anything else.
fn base64(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let b = s.as_bytes();
    if b.is_empty() || b.len() % 4 != 0 {
        return None;
    }
    let pad = b.iter().rev().take_while(|&&c| c == b'=').count();
    if pad > 2 {
        return None;
    }
    let mut out = Vec::with_capacity(b.len() / 4 * 3);
    for (n, chunk) in b.chunks_exact(4).enumerate() {
        let last = n == b.len() / 4 - 1;
        let mut v = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            let d = if c == b'=' && last && i >= 4 - pad {
                0
            } else {
                u32::from(val(c)?)
            };
            v = (v << 6) | d;
        }
        let bytes = v.to_be_bytes();
        let keep = if last { 3 - pad } else { 3 };
        out.extend_from_slice(&bytes[1..=keep]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::{PublicKey, blake2b::blake2b512, ed25519, sha512::sha512, verify};

    fn unhex(s: &str) -> Vec<u8> {
        let s: String = s.split_whitespace().collect();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    fn hex(b: &[u8]) -> String {
        nabic_core::hash::hex(b)
    }

    #[test]
    fn sha512_vectors() {
        assert_eq!(
            hex(&sha512(&[b""])),
            "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce\
             47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e"
        );
        assert_eq!(
            hex(&sha512(&[b"abc"])),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
             2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
        // Two blocks; split parts hash as one.
        let m = b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmno\
                  ijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu";
        let want = "8e959b75dae313da8cf4f72814fc143f8f7779c6eb9f7fa17299aeadb6889018\
                    501d289e4900f7e4331b99dec4b5433ac7d329eeb6dd26545e96e55b874be909";
        assert_eq!(hex(&sha512(&[m])), want);
        assert_eq!(hex(&sha512(&[&m[..5], &m[5..100], &m[100..]])), want);
    }

    #[test]
    fn blake2b_vectors() {
        assert_eq!(
            hex(&blake2b512(b"")),
            "786a02f742015903c6c6fd852552d272912f4740e15847618a86e217f71f5419\
             d25e1031afee585313896444934eb04b903a685b1448b755d56f701afe9be2ce"
        );
        // RFC 7693 appendix A.
        assert_eq!(
            hex(&blake2b512(b"abc")),
            "ba80a53f981c4d0d6a2797b69f12f6e94c212f14685ac4b74b12bb6fdbffa2d1\
             7d87c5392aab792dc252d5de4533cc9518d38aa8dbf1925ab92386edd4009923"
        );
        // Exactly one block, and one byte over.
        let m = [0x61u8; 129];
        assert_eq!(
            hex(&blake2b512(&m[..128])),
            include_str!("../testdata/blake2b-128a.hex").trim()
        );
        assert_eq!(
            hex(&blake2b512(&m)),
            include_str!("../testdata/blake2b-129a.hex").trim()
        );
    }

    /// Every length across the padding and block edges of both hashes.
    #[test]
    fn hash_lengths() {
        for l in include_str!("../testdata/lengths.txt").lines() {
            let [n, sha, blake] = l.split(' ').collect::<Vec<_>>()[..] else {
                continue;
            };
            let Ok(n) = n.parse::<usize>() else { continue };
            let m: Vec<u8> = (0..n).map(|i| (i % 251) as u8).collect();
            assert_eq!(hex(&sha512(&[&m])), sha, "sha512 of {n} bytes");
            assert_eq!(hex(&blake2b512(&m)), blake, "blake2b512 of {n} bytes");
        }
    }

    /// RFC 8032 section 7.1, tests 1, 2, 3 and 1024.
    const RFC8032: [(&str, &str, &str); 3] = [
        (
            "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
            "",
            "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e06522490155\
             5fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
        ),
        (
            "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c",
            "72",
            "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da\
             085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00",
        ),
        (
            "fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025",
            "af82",
            "6291d657deec24024827e69c3abe01a30ce548a284743a445e3680d7db5ac3ac\
             18ff9b538d16f290ae67f760984dc6594a7c15e9716ed28dc027beceea1ec40a",
        ),
    ];

    fn check(pk: &str, msg: &[u8], sig: &str) {
        let pk: [u8; 32] = unhex(pk).try_into().unwrap();
        let sig: [u8; 64] = unhex(sig).try_into().unwrap();
        assert!(ed25519::verify(&pk, msg, &sig));
        // A flipped bit anywhere fails.
        for i in [0, 31, 32, 63] {
            let mut bad = sig;
            bad[i] ^= 1;
            assert!(!ed25519::verify(&pk, msg, &bad), "sig bit {i}");
        }
        let mut m = msg.to_vec();
        m.push(0);
        assert!(!ed25519::verify(&pk, &m, &sig));
        let mut other = pk;
        other[0] ^= 1;
        assert!(!ed25519::verify(&other, msg, &sig));
    }

    #[test]
    fn ed25519_rfc8032() {
        for (pk, msg, sig) in RFC8032 {
            check(pk, &unhex(msg), sig);
        }
        check(
            "278117fc144c72340f67d0f2316e8386ceffbf2b2428c9c51fef7c597f1d426e",
            &unhex(include_str!("../testdata/rfc8032-1024.hex")),
            "0aab4c900501b3e24d7cdf4663326a3a87df5e4843b2cbdb67cbf6e460fec350\
             aa5371b1508f9f4528ecea23c436d94b5e8fcd4f681e30a6ac00a9704a188a03",
        );
    }

    #[test]
    fn ed25519_rejects_s_plus_l() {
        // Test 1's S + L verifies the same equation, but is not canonical.
        let (pk, _, sig) = RFC8032[0];
        let pk: [u8; 32] = unhex(pk).try_into().unwrap();
        let mut sig: [u8; 64] = unhex(sig).try_into().unwrap();
        let l = unhex("edd3f55c1a631258d69cf7a2def9de1400000000000000000000000000000010");
        let mut carry = 0u16;
        for i in 0..32 {
            let v = u16::from(sig[32 + i]) + u16::from(l[i]) + carry;
            sig[32 + i] = v as u8;
            carry = v >> 8;
        }
        assert_eq!(carry, 0);
        assert!(!ed25519::verify(&pk, b"", &sig));
    }

    const SUMS: &[u8] = include_bytes!("../testdata/SHA256SUMS");
    const PUB: &str = include_str!("../testdata/test.pub");

    #[test]
    fn minisign_prehashed_and_legacy() {
        let pk = PublicKey::parse(PUB).unwrap();
        assert_eq!(pk.id_hex(), "B6C6F4D188A666A9");
        for sig in [
            include_str!("../testdata/prehashed.minisig"),
            include_str!("../testdata/legacy.minisig"),
        ] {
            assert_eq!(
                verify(&pk, SUMS, sig, "SHA256SUMS").unwrap(),
                "nabi 0.1.0 SHA256SUMS"
            );
            // A changed file, signature or trusted comment fails.
            let mut m = SUMS.to_vec();
            m[0] ^= 1;
            assert!(verify(&pk, &m, sig, "SHA256SUMS").is_err());
            let forged = sig.replace("nabi 0.1.0", "nabi 0.1.1");
            let e = verify(&pk, SUMS, &forged, "SHA256SUMS").unwrap_err();
            assert!(e.contains("trusted comment"), "{e}");
            let lines: Vec<&str> = sig.lines().collect();
            let mut body = super::base64(lines[1]).unwrap();
            body[20] ^= 1;
            let b64 = super::encode64(&body);
            let tampered = sig.replace(lines[1], &b64);
            assert!(verify(&pk, SUMS, &tampered, "SHA256SUMS").is_err());
            assert!(verify(&pk, SUMS, &sig.replace('\n', "\r\n"), "SHA256SUMS").is_ok());
            assert!(verify(&pk, SUMS, &lines[..3].join("\n"), "SHA256SUMS").is_err());
        }
    }

    #[test]
    fn minisign_other_key() {
        let pk = PublicKey::parse(super::RELEASE_KEY).unwrap();
        assert_eq!(pk.id_hex(), "B33E6FE61BE5314C");
        let e = verify(
            &pk,
            SUMS,
            include_str!("../testdata/prehashed.minisig"),
            "SHA256SUMS",
        )
        .unwrap_err();
        assert!(e.contains("signed with key B6C6F4D188A666A9"), "{e}");
        // Same id, other key bytes: the signature does not verify.
        let mut other = PublicKey::parse(PUB).unwrap();
        other.key[3] ^= 1;
        assert!(
            verify(
                &other,
                SUMS,
                include_str!("../testdata/legacy.minisig"),
                "S"
            )
            .is_err()
        );
    }

    #[test]
    fn public_key_forms() {
        let line = PUB.lines().nth(1).unwrap();
        assert_eq!(PublicKey::parse(line), PublicKey::parse(PUB));
        assert!(PublicKey::parse("RWQ").is_err());
        assert!(PublicKey::parse("").is_err());
        // A secret key or signature line is not a public key.
        assert!(PublicKey::parse(include_str!("../testdata/legacy.minisig")).is_err());
    }

    #[test]
    fn sums_lines() {
        let sums = "AB01  a.tar.gz\n\
                    0123456789abcdef0123456789ABCDEF0123456789abcdef0123456789abcdef  nabi.tar.gz\r\n\
                    1111111111111111111111111111111111111111111111111111111111111111 *b.zip\n";
        assert_eq!(super::digest_of(sums, "a.tar.gz"), None);
        assert_eq!(
            super::digest_of(sums, "nabi.tar.gz").unwrap(),
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        );
        assert!(super::digest_of(sums, "b.zip").is_some());
        assert_eq!(super::digest_of(sums, "nabi.tar"), None);
    }

    #[test]
    fn base64_strict() {
        assert_eq!(super::base64("YWJj").unwrap(), b"abc");
        assert_eq!(super::base64("YWI=").unwrap(), b"ab");
        assert_eq!(super::base64("YQ==").unwrap(), b"a");
        assert!(super::base64("YQ=").is_none());
        assert!(super::base64("Y=Q=").is_none());
        assert!(super::base64("YW*j").is_none());
    }
}

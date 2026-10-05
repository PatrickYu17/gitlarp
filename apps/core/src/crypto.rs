//! Payload encryption for schedule records. AES-256-GCM, key derived
//! from the shared secret via SHA-256; the stored form is
//! `v1.<base64 iv>.<base64 ciphertext+tag>`, byte-compatible with
//! the old TS implementation, so existing stored records decrypt.

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use base64::Engine;
use serde::de::DeserializeOwned;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::http::Runtime;
use crate::Error;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

fn cipher(secret: &str) -> Aes256Gcm {
    Aes256Gcm::new_from_slice(&Sha256::digest(secret.as_bytes()))
        .expect("sha256 is a valid AES-256 key")
}

/// 12-byte AES-GCM nonce from OS entropy. `rt` stays in the signature
/// for call-site compatibility but is deliberately ignored: nonces
/// must never come from `Runtime::random` (fastrand/shell sources are
/// not crypto-secure, and GCM nonce reuse is catastrophic). Hard-fails
/// rather than ever encrypting with a repeatable nonce.
pub fn random_iv(_rt: &dyn Runtime) -> [u8; 12] {
    let mut iv = [0u8; 12];
    getrandom::getrandom(&mut iv).expect("OS entropy unavailable");
    iv
}

pub fn encrypt_json(
    secret: &str,
    value: &impl Serialize,
    iv: &[u8; 12],
) -> Result<String, Error> {
    let pt = serde_json::to_vec(value)
        .map_err(|e| Error::new(500, format!("serialize: {e}")))?;
    let ct = cipher(secret)
        .encrypt(Nonce::from_slice(iv), pt.as_ref())
        .map_err(|_| Error::new(500, "encrypt failed"))?;
    Ok(format!("v1.{}.{}", B64.encode(iv), B64.encode(ct)))
}

pub fn decrypt_json<T: DeserializeOwned>(secret: &str, blob: &str) -> Result<T, Error> {
    let mut parts = blob.split('.');
    let (Some(v), Some(iv), Some(ct), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(Error::new(500, "corrupt payload"));
    };
    if v != "v1" {
        return Err(Error::new(500, "corrupt payload"));
    }
    let iv = B64.decode(iv).map_err(|_| Error::new(500, "corrupt payload"))?;
    // AES-GCM needs a 12-byte nonce; from_slice would panic on any other
    // length, and a malformed stored record must fail gracefully, not
    // take the process down.
    if iv.len() != 12 {
        return Err(Error::new(500, "corrupt payload"));
    }
    let ct = B64.decode(ct).map_err(|_| Error::new(500, "corrupt payload"))?;
    let pt = cipher(secret)
        .decrypt(Nonce::from_slice(&iv), ct.as_ref())
        .map_err(|_| Error::new(500, "corrupt payload"))?;
    serde_json::from_slice(&pt).map_err(|_| Error::new(500, "corrupt payload"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::HttpRequest;

    const SECRET: &str = "s3cret";

    /// Runtime whose `random` panics, proving nonces never touch it.
    struct NoRandomRt;

    impl Runtime for NoRandomRt {
        fn fetch(
            &self,
            _req: HttpRequest,
        ) -> crate::http::BoxFut<Result<crate::http::HttpResponse, Error>> {
            Box::pin(std::future::ready(Err(Error::new(500, "unused"))))
        }
        fn sleep(&self, _ms: u64) -> crate::http::BoxFut<()> {
            Box::pin(std::future::ready(()))
        }
        fn random(&self, _buf: &mut [u8]) {
            panic!("GCM nonces must not come from Runtime::random");
        }
    }

    #[test]
    fn nonces_come_from_os_entropy() {
        let rt = NoRandomRt;
        let a = random_iv(&rt);
        let b = random_iv(&rt);
        assert_ne!(a, b, "two fresh nonces must differ (12 random bytes, ~2^-96 collision odds)");
    }

    #[test]
    fn roundtrip() {
        let value = serde_json::json!({ "pat": "x", "n": 3 });
        let blob = encrypt_json(SECRET, &value, &[7u8; 12]).unwrap();
        assert!(blob.starts_with("v1."));
        let back: serde_json::Value = decrypt_json(SECRET, &blob).unwrap();
        assert_eq!(back, value);
    }

    #[test]
    fn wrong_secret_fails() {
        let blob = encrypt_json(SECRET, &serde_json::json!({ "a": 1 }), &[1u8; 12]).unwrap();
        assert!(decrypt_json::<serde_json::Value>("other", &blob).is_err());
    }

    #[test]
    fn corrupt_fails() {
        for bad in [
            "",
            "v2.A.B",
            "v1.!!",
            "v1.YWJj.!!",
            // valid base64 everywhere, but the IV decodes to 3 bytes;
            // must be rejected, not panic
            "v1.YWJj.aGVsbG8=",
            "v1.YWJj",
        ] {
            assert!(decrypt_json::<serde_json::Value>(SECRET, bad).is_err(), "case: {bad}");
        }
    }

    /// Vector produced by the old TS implementation (WebCrypto, fixed
    /// iv), proving stored payloads decrypt after the port.
    #[test]
    fn ts_compat() {
        const TS_BLOB: &str = "v1.AAECAwQFBgcICQoL.OgT65czAHLSr2cUe9dy2WLhcod0jkzMgCLMMuD0xGRSI5z+5cil2x2Lf8k8r8JNfb6ydcxhBDAt8UCC1Kw8EyKAFzxAsDOjeTHpzidfoU2zHLE4yOdkjtUdp/RvvdPkCTTYZE12SZnUzAM7Un4qSFL1E8EgipUQlu96dNmE4ZLjqbo6o2k0E2g==";
        let v: serde_json::Value = decrypt_json(SECRET, TS_BLOB).unwrap();
        assert_eq!(v["pat"], "ghp_test");
        assert_eq!(v["spec"]["min"], 1);
        assert_eq!(v["spec"]["weekends"], false);
        assert_eq!(v["spec"]["catchup"], 7);
        assert_eq!(v["lastRun"], "2026-08-01");
        // note: re-encrypting need NOT reproduce the blob byte-for-byte
        // (JSON key order differs); only decrypt compatibility matters.
    }
}

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use rsa::RsaPrivateKey;
use rsa::pss::{BlindedSigningKey, Signature};
use rsa::signature::{RandomizedSigner, SignatureEncoding};
use sha2::Sha256;
use std::sync::Arc;

use crate::kalshi::error::KalshiError;

/// Holds the RSA private key and API key for signing Kalshi requests.
#[derive(Clone)]
pub struct KalshiAuth {
    api_key: String,
    private_key: Arc<RsaPrivateKey>,
}

impl KalshiAuth {
    /// Load auth credentials from API key and PEM file path.
    pub fn new(api_key: String, private_key_path: &str) -> Result<Self, KalshiError> {
        let pem_bytes = std::fs::read_to_string(private_key_path).map_err(|e| {
            KalshiError::SigningError(format!(
                "failed to read private key at {private_key_path}: {e}"
            ))
        })?;

        use rsa::pkcs8::DecodePrivateKey;
        let private_key = RsaPrivateKey::from_pkcs8_pem(&pem_bytes)
            .or_else(|_| {
                // Fall back to PKCS#1 format (BEGIN RSA PRIVATE KEY)
                use rsa::pkcs1::DecodeRsaPrivateKey;
                RsaPrivateKey::from_pkcs1_pem(&pem_bytes)
            })
            .map_err(|e| {
                KalshiError::SigningError(format!("failed to parse PEM private key: {e}"))
            })?;

        Ok(Self {
            api_key,
            private_key: Arc::new(private_key),
        })
    }

    /// Return the API key (for WS auth which only needs the key).
    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    /// Sign a request and return the three auth headers.
    ///
    /// Message format: `{timestamp_ms}{METHOD}{path}`
    pub fn sign_request(&self, method: &str, path: &str) -> Result<AuthHeaders, KalshiError> {
        let timestamp_ms = chrono::Utc::now().timestamp_millis();
        let message = format!("{timestamp_ms}{method}{path}");

        let signing_key = BlindedSigningKey::<Sha256>::new((*self.private_key).clone());
        let mut rng = rsa::rand_core::OsRng;
        let signature: Signature = signing_key.sign_with_rng(&mut rng, message.as_bytes());
        let encoded_signature = BASE64.encode(signature.to_bytes());

        Ok(AuthHeaders {
            api_key: self.api_key.clone(),
            signature: encoded_signature,
            timestamp: timestamp_ms.to_string(),
        })
    }
}

/// The three headers required for authenticated Kalshi requests.
pub struct AuthHeaders {
    pub api_key: String,
    pub signature: String,
    pub timestamp: String,
}

impl std::fmt::Debug for KalshiAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KalshiAuth")
            .field("api_key", &"[redacted]")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::pkcs8::EncodePrivateKey;
    use std::io::Write;

    /// Generate a test RSA keypair and write the PEM to a temp file.
    fn write_test_key_pkcs8() -> (tempfile::NamedTempFile, RsaPrivateKey) {
        let mut rng = rsa::rand_core::OsRng;
        let key = RsaPrivateKey::new(&mut rng, 2048).expect("keygen failed");
        let pem = key
            .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
            .expect("pem encode failed");
        let mut f = tempfile::NamedTempFile::new().expect("tempfile failed");
        f.write_all(pem.as_bytes()).expect("write failed");
        f.flush().expect("flush failed");
        (f, key)
    }

    /// Generate a PKCS#1 PEM file (BEGIN RSA PRIVATE KEY).
    fn write_test_key_pkcs1() -> tempfile::NamedTempFile {
        use rsa::pkcs1::EncodeRsaPrivateKey;
        let mut rng = rsa::rand_core::OsRng;
        let key = RsaPrivateKey::new(&mut rng, 2048).expect("keygen failed");
        let pem = key
            .to_pkcs1_pem(rsa::pkcs8::LineEnding::LF)
            .expect("pem encode failed");
        let mut f = tempfile::NamedTempFile::new().expect("tempfile failed");
        f.write_all(pem.as_bytes()).expect("write failed");
        f.flush().expect("flush failed");
        f
    }

    #[test]
    fn load_pkcs8_key() {
        let (f, _) = write_test_key_pkcs8();
        let auth = KalshiAuth::new("my-api-key".into(), f.path().to_str().unwrap());
        assert!(auth.is_ok());
        assert_eq!(auth.unwrap().api_key(), "my-api-key");
    }

    #[test]
    fn load_pkcs1_key() {
        let f = write_test_key_pkcs1();
        let auth = KalshiAuth::new("my-api-key".into(), f.path().to_str().unwrap());
        assert!(auth.is_ok());
    }

    #[test]
    fn invalid_key_path_returns_error() {
        let result = KalshiAuth::new("key".into(), "/nonexistent/path.pem");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, KalshiError::SigningError(_)));
    }

    #[test]
    fn invalid_pem_content_returns_error() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"not a real PEM file").unwrap();
        f.flush().unwrap();
        let result = KalshiAuth::new("key".into(), f.path().to_str().unwrap());
        assert!(result.is_err());
    }

    #[test]
    fn sign_request_produces_valid_output() {
        let (f, _) = write_test_key_pkcs8();
        let auth = KalshiAuth::new("test-key".into(), f.path().to_str().unwrap()).unwrap();

        let headers = auth.sign_request("GET", "/trade-api/v2/markets").unwrap();

        // API key is echoed back
        assert_eq!(headers.api_key, "test-key");

        // Timestamp is a valid integer (milliseconds since epoch)
        let ts: i64 = headers.timestamp.parse().expect("timestamp should be numeric");
        assert!(ts > 1_700_000_000_000); // after ~Nov 2023

        // Signature is valid base64
        let decoded = BASE64.decode(&headers.signature);
        assert!(decoded.is_ok(), "signature must be valid base64");
        assert!(!decoded.unwrap().is_empty(), "signature must not be empty");
    }

    #[test]
    fn sign_request_different_methods_produce_different_signatures() {
        let (f, _) = write_test_key_pkcs8();
        let auth = KalshiAuth::new("key".into(), f.path().to_str().unwrap()).unwrap();

        let h1 = auth.sign_request("GET", "/path").unwrap();
        let h2 = auth.sign_request("POST", "/path").unwrap();

        // Different methods should produce different signatures (with overwhelming probability)
        assert_ne!(h1.signature, h2.signature);
    }

    #[test]
    fn sign_request_different_paths_produce_different_signatures() {
        let (f, _) = write_test_key_pkcs8();
        let auth = KalshiAuth::new("key".into(), f.path().to_str().unwrap()).unwrap();

        let h1 = auth.sign_request("GET", "/path1").unwrap();
        let h2 = auth.sign_request("GET", "/path2").unwrap();

        assert_ne!(h1.signature, h2.signature);
    }

    #[test]
    fn sign_request_verifies_with_public_key() {
        use rsa::pss::VerifyingKey;
        use rsa::signature::Verifier;

        let (f, private_key) = write_test_key_pkcs8();
        let auth = KalshiAuth::new("key".into(), f.path().to_str().unwrap()).unwrap();

        let headers = auth.sign_request("GET", "/test").unwrap();

        // Reconstruct message as the code does
        let message = format!("{}GET/test", headers.timestamp);
        let sig_bytes = BASE64.decode(&headers.signature).unwrap();
        let signature = Signature::try_from(sig_bytes.as_slice()).unwrap();

        let verifying_key = VerifyingKey::<Sha256>::new(private_key.to_public_key());
        assert!(verifying_key.verify(message.as_bytes(), &signature).is_ok());
    }

    #[test]
    fn debug_redacts_api_key() {
        let (f, _) = write_test_key_pkcs8();
        let auth = KalshiAuth::new("super-secret-key".into(), f.path().to_str().unwrap()).unwrap();
        let debug = format!("{:?}", auth);
        assert!(debug.contains("[redacted]"));
        assert!(!debug.contains("super-secret-key"));
    }
}

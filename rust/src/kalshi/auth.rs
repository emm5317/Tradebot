use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use openssl::hash::MessageDigest;
use openssl::pkey::PKey;
use openssl::sign::Signer;
use std::sync::Arc;

use crate::kalshi::error::KalshiError;

/// Holds the RSA private key and API key for signing Kalshi requests.
#[derive(Clone)]
pub struct KalshiAuth {
    api_key: String,
    private_key: Arc<PKey<openssl::pkey::Private>>,
}

impl KalshiAuth {
    /// Load auth credentials from API key and PEM file path.
    pub fn new(api_key: String, private_key_path: &str) -> Result<Self, KalshiError> {
        let pem_bytes = std::fs::read(private_key_path).map_err(|e| {
            KalshiError::SigningError(format!("failed to read private key at {private_key_path}: {e}"))
        })?;

        let private_key = PKey::private_key_from_pem(&pem_bytes).map_err(|e| {
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
    pub fn sign_request(
        &self,
        method: &str,
        path: &str,
    ) -> Result<AuthHeaders, KalshiError> {
        let timestamp_ms = chrono::Utc::now().timestamp_millis();
        let message = format!("{timestamp_ms}{method}{path}");

        let mut signer = Signer::new(MessageDigest::sha256(), &self.private_key)
            .map_err(|e| KalshiError::SigningError(e.to_string()))?;

        signer
            .set_rsa_padding(openssl::rsa::Padding::PKCS1_PSS)
            .map_err(|e| KalshiError::SigningError(e.to_string()))?;
        signer
            .set_rsa_pss_saltlen(openssl::sign::RsaPssSaltlen::DIGEST_LENGTH)
            .map_err(|e| KalshiError::SigningError(e.to_string()))?;

        signer
            .update(message.as_bytes())
            .map_err(|e| KalshiError::SigningError(e.to_string()))?;

        let signature = signer
            .sign_to_vec()
            .map_err(|e| KalshiError::SigningError(e.to_string()))?;

        let encoded_signature = BASE64.encode(&signature);

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

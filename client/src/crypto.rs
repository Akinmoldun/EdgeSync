use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("Invalid key format: {0}")]
    KeyError(String),
    #[error("Signature verification failed: {0}")]
    VerificationError(String),
}

/// DeviceIdentity stores the persistent cryptographic credentials of the edge node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceIdentity {
    pub device_id: Uuid,
    pub secret_key_hex: String,
    pub public_key_hex: String,
}

/// Keypair holds the Ed25519 signing and verifying keys and associated device UUID.
pub struct Keypair {
    pub device_id: Uuid,
    pub signing_key: SigningKey,
    pub verifying_key: VerifyingKey,
}

impl Keypair {
    /// Generates a new cryptographic keypair using cryptographically secure random bytes from UUIDv4.
    pub fn generate() -> Self {
        let device_id = Uuid::new_v4();
        let mut seed = [0u8; 32];
        let u1 = Uuid::new_v4();
        let u2 = Uuid::new_v4();
        seed[..16].copy_from_slice(u1.as_bytes());
        seed[16..].copy_from_slice(u2.as_bytes());

        let signing_key = SigningKey::from_bytes(&seed);
        let verifying_key = signing_key.verifying_key();
        Self {
            device_id,
            signing_key,
            verifying_key,
        }
    }

    /// Reconstructs a keypair from raw 32-byte secret seed and assigned device ID.
    pub fn from_bytes(device_id: Uuid, bytes: &[u8; 32]) -> Self {
        let signing_key = SigningKey::from_bytes(bytes);
        let verifying_key = signing_key.verifying_key();
        Self {
            device_id,
            signing_key,
            verifying_key,
        }
    }

    /// Returns public key as a 32-byte slice.
    pub fn public_key_bytes(&self) -> [u8; 32] {
        self.verifying_key.to_bytes()
    }

    /// Returns the hex-encoded string of the public key.
    pub fn public_key_hex(&self) -> String {
        hex_encode(&self.public_key_bytes())
    }

    /// Returns the hex-encoded string of the secret key.
    pub fn secret_key_hex(&self) -> String {
        hex_encode(&self.signing_key.to_bytes())
    }

    /// Signs an arbitrary message using the signing key.
    pub fn sign(&self, message: &[u8]) -> Vec<u8> {
        let signature: Signature = self.signing_key.sign(message);
        signature.to_bytes().to_vec()
    }

    /// Verifies that a signature is valid for a given message using the verifying key.
    pub fn verify(&self, message: &[u8], signature_bytes: &[u8]) -> Result<(), CryptoError> {
        if signature_bytes.len() != 64 {
            return Err(CryptoError::VerificationError(
                "Invalid signature length: must be 64 bytes".to_string(),
            ));
        }
        let mut sig_arr = [0u8; 64];
        sig_arr.copy_from_slice(signature_bytes);
        let signature = Signature::from_bytes(&sig_arr);

        self.verifying_key
            .verify(message, &signature)
            .map_err(|e| CryptoError::VerificationError(e.to_string()))
    }

    /// Saves the device identity and private key securely to disk.
    pub fn save_to_file<P: AsRef<Path>>(&self, path: P) -> Result<(), CryptoError> {
        let identity = DeviceIdentity {
            device_id: self.device_id,
            secret_key_hex: self.secret_key_hex(),
            public_key_hex: self.public_key_hex(),
        };
        let json_str = serde_json::to_string_pretty(&identity)?;
        fs::write(path, json_str)?;
        Ok(())
    }

    /// Loads the device identity and keypair from disk.
    pub fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self, CryptoError> {
        let content = fs::read_to_string(path)?;
        let identity: DeviceIdentity = serde_json::from_str(&content)?;

        let secret_bytes = hex_decode(&identity.secret_key_hex).map_err(|e| {
            CryptoError::KeyError(format!("Failed to decode secret key hex: {}", e))
        })?;

        if secret_bytes.len() != 32 {
            return Err(CryptoError::KeyError(
                "Decoded secret key must be exactly 32 bytes".to_string(),
            ));
        }

        let mut seed = [0u8; 32];
        seed.copy_from_slice(&secret_bytes);

        Ok(Self::from_bytes(identity.device_id, &seed))
    }

    /// Loads keypair from disk if it exists; otherwise generates and saves a new one.
    pub fn load_or_generate<P: AsRef<Path>>(path: P) -> Result<Self, CryptoError> {
        let p = path.as_ref();
        if p.exists() {
            Self::load_from_file(p)
        } else {
            let keypair = Self::generate();
            keypair.save_to_file(p)?;
            Ok(keypair)
        }
    }
}

pub fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

pub fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    if s.len() % 2 != 0 {
        return Err("Invalid hex string length".to_string());
    }
    let mut bytes = Vec::with_capacity(s.len() / 2);
    for i in (0..s.len()).step_by(2) {
        let byte = u8::from_str_radix(&s[i..i + 2], 16)
            .map_err(|e| format!("Invalid hex character at {}: {}", i, e))?;
        bytes.push(byte);
    }
    Ok(bytes)
}

/// Decodes standard Base64 string into bytes without external dependencies.
pub fn base64_decode(input: &str) -> Result<Vec<u8>, String> {
    let mut table = [255u8; 256];
    for (i, &b) in b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
        .iter()
        .enumerate()
    {
        table[b as usize] = i as u8;
    }

    let clean: Vec<u8> = input
        .bytes()
        .filter(|&b| b != b'=' && !b.is_ascii_whitespace())
        .collect();
    let mut out = Vec::with_capacity((clean.len() * 3) / 4);

    let chunks = clean.chunks_exact(4);
    let remainder = chunks.remainder();

    for chunk in chunks {
        let b0 = table[chunk[0] as usize];
        let b1 = table[chunk[1] as usize];
        let b2 = table[chunk[2] as usize];
        let b3 = table[chunk[3] as usize];
        if b0 == 255 || b1 == 255 || b2 == 255 || b3 == 255 {
            return Err("Invalid base64 character".into());
        }
        out.push((b0 << 2) | (b1 >> 4));
        out.push((b1 << 4) | (b2 >> 2));
        out.push((b2 << 6) | b3);
    }

    match remainder.len() {
        2 => {
            let b0 = table[remainder[0] as usize];
            let b1 = table[remainder[1] as usize];
            if b0 == 255 || b1 == 255 {
                return Err("Invalid base64 character".into());
            }
            out.push((b0 << 2) | (b1 >> 4));
        }
        3 => {
            let b0 = table[remainder[0] as usize];
            let b1 = table[remainder[1] as usize];
            let b2 = table[remainder[2] as usize];
            if b0 == 255 || b1 == 255 || b2 == 255 {
                return Err("Invalid base64 character".into());
            }
            out.push((b0 << 2) | (b1 >> 4));
            out.push((b1 << 4) | (b2 >> 2));
        }
        1 => return Err("Invalid base64 remainder length".into()),
        _ => {}
    }

    Ok(out)
}

/// Parses signature bytes from hex string, base64 string, or JSON byte array.
pub fn parse_signature(val: &serde_json::Value) -> Result<Vec<u8>, String> {
    match val {
        serde_json::Value::String(s) => {
            // Try hex first
            if let Ok(bytes) = hex_decode(s) {
                if bytes.len() == 64 {
                    return Ok(bytes);
                }
            }
            // Try base64
            if let Ok(bytes) = base64_decode(s) {
                if bytes.len() == 64 {
                    return Ok(bytes);
                }
            }
            Err(format!(
                "Signature string is neither valid 64-byte hex nor base64: {}",
                s
            ))
        }
        serde_json::Value::Array(arr) => {
            let mut bytes = Vec::with_capacity(arr.len());
            for v in arr {
                if let Some(n) = v.as_u64() {
                    bytes.push(n as u8);
                } else {
                    return Err("Signature array contains non-integer element".into());
                }
            }
            if bytes.len() == 64 {
                Ok(bytes)
            } else {
                Err(format!(
                    "Signature array length is {}, expected 64",
                    bytes.len()
                ))
            }
        }
        _ => Err("Invalid signature format: expected string or byte array".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_keypair_generation_and_signing() {
        let keypair = Keypair::generate();
        let message = b"sample event message";
        let signature = keypair.sign(message);

        assert_eq!(signature.len(), 64);
        assert!(keypair.verify(message, &signature).is_ok());

        let wrong_msg = b"tampered message";
        assert!(keypair.verify(wrong_msg, &signature).is_err());
    }

    #[test]
    fn test_keypair_file_persistence() {
        let temp_file = NamedTempFile::new().expect("create temp file");
        let path = temp_file.path();

        let original = Keypair::generate();
        original.save_to_file(path).expect("save keypair");

        let loaded = Keypair::load_from_file(path).expect("load keypair");
        assert_eq!(original.device_id, loaded.device_id);
        assert_eq!(original.public_key_hex(), loaded.public_key_hex());
        assert_eq!(original.secret_key_hex(), loaded.secret_key_hex());

        // Verify loaded key signs and verifies identically
        let msg = b"persistence test";
        let sig = loaded.sign(msg);
        assert!(original.verify(msg, &sig).is_ok());
    }

    #[test]
    fn test_load_or_generate() {
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let path = temp_dir.path().join("device.key");

        assert!(!path.exists());
        let first = Keypair::load_or_generate(&path).expect("generate key");
        assert!(path.exists());

        let second = Keypair::load_or_generate(&path).expect("load key");
        assert_eq!(first.device_id, second.device_id);
        assert_eq!(first.public_key_hex(), second.public_key_hex());
    }
}

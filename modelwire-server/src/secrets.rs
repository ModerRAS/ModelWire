//! Managed provider secret encryption helpers.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use sha2::{Digest, Sha256};

use modelwire_core::error::{Error, ErrorKind};

const PREFIX: &str = "mwenc:v1:";

fn derive_key(secret: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    let digest = hasher.finalize();
    let mut key = [0u8; 32];
    key.copy_from_slice(&digest[..32]);
    key
}

pub fn encrypt_managed_key(plaintext: &str, secret: &str) -> Result<String, Error> {
    let key = derive_key(secret);
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|error| {
        Error::new(
            ErrorKind::InternalError,
            format!("Failed to initialize managed key cipher: {error}"),
        )
    })?;

    let nonce_bytes: [u8; 12] = rand::random();
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|_| Error::new(ErrorKind::InternalError, "Managed key encryption failed"))?;

    let mut blob = Vec::with_capacity(12 + ciphertext.len());
    blob.extend_from_slice(&nonce_bytes);
    blob.extend_from_slice(&ciphertext);
    Ok(format!("{PREFIX}{}", STANDARD.encode(blob)))
}

pub fn decrypt_managed_key(ciphertext: &str, secret: &str) -> Result<String, Error> {
    let encoded = ciphertext
        .strip_prefix(PREFIX)
        .ok_or_else(|| Error::new(ErrorKind::InternalError, "Invalid managed key ciphertext"))?;
    let blob = STANDARD
        .decode(encoded)
        .map_err(|_| Error::new(ErrorKind::InternalError, "Invalid managed key ciphertext"))?;
    if blob.len() < 13 {
        return Err(Error::new(
            ErrorKind::InternalError,
            "Invalid managed key ciphertext",
        ));
    }
    let (nonce_bytes, payload) = blob.split_at(12);

    let key = derive_key(secret);
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|error| {
        Error::new(
            ErrorKind::InternalError,
            format!("Failed to initialize managed key cipher: {error}"),
        )
    })?;
    let nonce = Nonce::from_slice(nonce_bytes);
    let plaintext = cipher
        .decrypt(nonce, payload)
        .map_err(|_| Error::new(ErrorKind::InternalError, "Managed key decryption failed"))?;
    String::from_utf8(plaintext).map_err(|_| {
        Error::new(
            ErrorKind::InternalError,
            "Managed key plaintext is not UTF-8",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_key_roundtrip() {
        let secret = "enc-master-secret";
        let plaintext = "sk-managed-123";
        let ciphertext = encrypt_managed_key(plaintext, secret).unwrap();
        assert!(ciphertext.starts_with(PREFIX));
        assert_ne!(ciphertext, plaintext);
        let restored = decrypt_managed_key(&ciphertext, secret).unwrap();
        assert_eq!(restored, plaintext);
    }
}

//! History encryption and the per-database key in the user's Secret Service.
use crate::types::DaemonError;
use aes_gcm::aead::rand_core::RngCore;
use aes_gcm::aead::{Aead, OsRng, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use secret_service::{EncryptionType, SecretService};
use std::collections::HashMap;
use zeroize::Zeroizing;

const VERSION: u8 = 1;
const NONCE_LEN: usize = 12;

pub(crate) struct HistoryKey(Zeroizing<[u8; 32]>);

impl HistoryKey {
    pub(crate) fn seal(&self, id: &str, field: &str, plaintext: &[u8]) -> Result<Vec<u8>, DaemonError> {
        let mut nonce = [0; NONCE_LEN];
        OsRng.try_fill_bytes(&mut nonce)
            .map_err(|_| DaemonError::Internal("could not generate an encryption nonce".into()))?;
        let cipher = Aes256Gcm::new((&*self.0).into());
        let ciphertext = cipher.encrypt((&nonce).into(), Payload { msg: plaintext, aad: &aad(id, field) })
            .map_err(|_| DaemonError::Internal("could not encrypt clipboard data".into()))?;
        let mut record = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
        record.push(VERSION);
        record.extend(nonce);
        record.extend(ciphertext);
        Ok(record)
    }

    pub(crate) fn open(&self, id: &str, field: &str, record: &[u8]) -> Result<Vec<u8>, DaemonError> {
        if record.len() < 1 + NONCE_LEN + 16 || record[0] != VERSION {
            return Err(DaemonError::Encrypted);
        }
        let cipher = Aes256Gcm::new((&*self.0).into());
        cipher.decrypt(Nonce::from_slice(&record[1..1 + NONCE_LEN]), Payload {
            msg: &record[1 + NONCE_LEN..],
            aad: &aad(id, field),
        }).map_err(|_| DaemonError::Encrypted)
    }

    pub(crate) fn protected_hash(&self, hash: &str) -> String {
        let key = Zeroizing::new(blake3::derive_key("clipboard-manager history dedup key v1", &*self.0));
        blake3::keyed_hash(&key, hash.as_bytes()).to_hex().to_string()
    }

    #[cfg(test)]
    pub(crate) fn fixture(bytes: [u8; 32]) -> Self {
        Self(Zeroizing::new(bytes))
    }
}

fn aad(id: &str, field: &str) -> Vec<u8> {
    let mut aad = b"clipboard-manager/history/v1".to_vec();
    aad.extend((id.len() as u64).to_le_bytes());
    aad.extend(id.as_bytes());
    aad.extend(field.as_bytes());
    aad
}

pub(crate) async fn keyring_key(
    connection: &zbus::Connection,
    id: &str,
    create: bool,
) -> Result<(HistoryKey, zbus::Proxy<'static>), DaemonError> {
    let locked = |_| DaemonError::Encrypted;
    let service = SecretService::connect_with_existing(EncryptionType::Dh, connection.clone())
        .await.map_err(|_| DaemonError::Encrypted)?;
    let attributes = HashMap::from([("application", "org.clipboardmanager.Daemon"), ("history-key-id", id)]);
    let results = service.search_items(attributes.clone()).await.map_err(|_| DaemonError::Encrypted)?;
    let item = match (results.unlocked.as_slice(), results.locked.as_slice()) {
        ([item], []) => (item.item_path.clone(), Zeroizing::new(item.get_secret().await.map_err(|_| DaemonError::Encrypted)?)),
        ([], []) if create => {
            let collection = service.get_default_collection().await.map_err(|_| DaemonError::Encrypted)?;
            if collection.is_locked().await.map_err(|_| DaemonError::Encrypted)? {
                return Err(DaemonError::Encrypted);
            }
            let mut key = Zeroizing::new(vec![0; 32]);
            OsRng.try_fill_bytes(&mut key)
                .map_err(|_| DaemonError::Internal("could not generate a history key".into()))?;
            let item = collection.create_item("Clipboard Manager history key", attributes, &key, false,
                "application/octet-stream").await.map_err(|_| DaemonError::Encrypted)?;
            (item.item_path.clone(), key)
        }
        _ => return Err(DaemonError::Encrypted),
    };
    let mut key = Zeroizing::new([0; 32]);
    if item.1.len() != key.len() {
        return Err(DaemonError::Encrypted);
    }
    key.copy_from_slice(&item.1);
    let proxy = zbus::proxy::Builder::<zbus::Proxy<'static>>::new(&connection)
        .destination("org.freedesktop.secrets").map_err(locked)?
        .path(item.0).map_err(locked)?
        .interface("org.freedesktop.Secret.Item").map_err(locked)?
        .cache_properties(zbus::proxy::CacheProperties::Yes)
        .build().await.map_err(locked)?;
    Ok((HistoryKey(key), proxy))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_authenticate_the_bytes_item_and_field() {
        let key = HistoryKey::fixture([9; 32]);
        let content = b"fixture";
        let mut record = key.seal("item", "payload:text/plain", content).unwrap();
        assert_eq!(key.open("item", "payload:text/plain", &record).unwrap(), content);
        assert_ne!(key.seal("item", "payload:text/plain", content).unwrap(), record);
        assert!(key.open("other-item", "payload:text/plain", &record).is_err());
        assert!(key.open("item", "payload:text/html", &record).is_err());
        let last = record.len() - 1;
        record[last] ^= 1;
        assert!(key.open("item", "payload:text/plain", &record).is_err());
    }
}

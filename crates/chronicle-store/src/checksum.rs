use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{Result, StoreError};

pub fn canonical_json<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(value)?)
}

pub fn checksum_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

pub fn verify_checksum(bytes: &[u8], expected: &str) -> Result<()> {
    let actual = checksum_bytes(bytes);
    if actual == expected {
        Ok(())
    } else {
        Err(StoreError::StableIdConflict {
            id: "checksum".to_owned(),
        })
    }
}

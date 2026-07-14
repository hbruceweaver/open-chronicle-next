use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::maintenance::ensure_normal_store_access;
use crate::{ManagedRoot, Result, StoreError};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreGeneration {
    pub schema_version: u32,
    pub generation: u64,
    pub epoch_id: Uuid,
}

impl StoreGeneration {
    pub fn initialize(root: &ManagedRoot) -> Result<Self> {
        if root.exists("store-generation")? {
            return Self::load(root);
        }
        ensure_normal_store_access(root)?;
        let generation = Self {
            schema_version: 1,
            generation: 1,
            epoch_id: Uuid::now_v7(),
        };
        generation.persist(root)?;
        Ok(generation)
    }

    pub fn load(root: &ManagedRoot) -> Result<Self> {
        let generation: Self = serde_json::from_slice(&root.read("store-generation")?)?;
        if generation.schema_version != 1 || generation.generation == 0 {
            return Err(StoreError::InvalidPath(
                "invalid store-generation document".to_owned(),
            ));
        }
        Ok(generation)
    }

    pub fn increment(&self, root: &ManagedRoot) -> Result<Self> {
        ensure_normal_store_access(root)?;
        let next = Self {
            schema_version: 1,
            generation: self
                .generation
                .checked_add(1)
                .ok_or_else(|| StoreError::InvalidPath("store generation overflow".to_owned()))?,
            epoch_id: Uuid::now_v7(),
        };
        next.persist(root)?;
        Ok(next)
    }

    pub fn ensure_current(&self, root: &ManagedRoot) -> Result<()> {
        let current = Self::load(root)?;
        if current == *self {
            Ok(())
        } else {
            Err(StoreError::StaleGeneration {
                expected: self.generation,
                actual: current.generation,
            })
        }
    }

    fn persist(&self, root: &ManagedRoot) -> Result<()> {
        let bytes = serde_json::to_vec(self)?;
        root.atomic_write("store-generation", &bytes)
    }
}

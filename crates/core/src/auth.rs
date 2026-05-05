//! Auth resolver trait + NoAuth reference impl.
//!
//! See docs/14-protocol-surface.md §"Auth model".

use std::collections::HashSet;

use crate::{Owner, Principal};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Credentials {
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub principal: Principal,
    pub accessible_owners: HashSet<Owner>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    #[error("authentication required")]
    AuthRequired,
    #[error("invalid credentials")]
    InvalidCredentials,
}

pub trait AuthResolver: Send + Sync {
    fn resolve(&self, creds: &Credentials) -> Result<Resolved, AuthError>;
}

#[derive(Debug, Clone)]
pub struct NoAuth {
    principal: Principal,
    owner: Owner,
}

impl NoAuth {
    pub fn new(principal: Principal, owner: Owner) -> Self {
        Self { principal, owner }
    }
}

impl AuthResolver for NoAuth {
    fn resolve(&self, _creds: &Credentials) -> Result<Resolved, AuthError> {
        let mut owners = HashSet::with_capacity(1);
        owners.insert(self.owner.clone());
        Ok(Resolved {
            principal: self.principal.clone(),
            accessible_owners: owners,
        })
    }
}

//! Where refresh tokens are kept: one entry per account.
//!
//! The daemon uses the Secret Service ([`SecretServiceStore`](super::SecretServiceStore));
//! there is deliberately no file-backed store (ADR-0010 R6, open-questions D3).

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Mutex, PoisonError};

use mydrive_core::ids::AccountId;

use super::error::StoreError;
use super::secret::RefreshToken;

/// A boxed, sendable future, so the auth traits can be used as trait objects.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Stores one refresh token per account.
pub trait TokenStore: Send + Sync {
    /// The account's refresh token, or `None` if there is none.
    fn load<'a>(
        &'a self,
        account: &'a AccountId,
    ) -> BoxFuture<'a, Result<Option<RefreshToken>, StoreError>>;

    /// Stores `token` as the account's refresh token, replacing any previous one. When this
    /// returns an error, the previous token must still be the stored one.
    fn save<'a>(
        &'a self,
        account: &'a AccountId,
        token: &'a RefreshToken,
    ) -> BoxFuture<'a, Result<(), StoreError>>;

    /// Removes the account's refresh token, if any (sign-out).
    fn delete<'a>(&'a self, account: &'a AccountId) -> BoxFuture<'a, Result<(), StoreError>>;
}

/// An in-memory store, for tests and for code that must not touch a keyring.
#[derive(Default)]
pub struct MemoryTokenStore {
    tokens: Mutex<HashMap<AccountId, RefreshToken>>,
}

impl fmt::Debug for MemoryTokenStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tokens = self.tokens.lock().unwrap_or_else(PoisonError::into_inner);
        f.debug_struct("MemoryTokenStore")
            .field("accounts", &tokens.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl MemoryTokenStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The stored token for `account`, without going through the async trait.
    #[must_use]
    pub fn get(&self, account: &AccountId) -> Option<RefreshToken> {
        self.tokens
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(account)
            .cloned()
    }

    /// How many accounts have a stored token.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tokens
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Whether no account has a stored token.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl TokenStore for MemoryTokenStore {
    fn load<'a>(
        &'a self,
        account: &'a AccountId,
    ) -> BoxFuture<'a, Result<Option<RefreshToken>, StoreError>> {
        Box::pin(async move { Ok(self.get(account)) })
    }

    fn save<'a>(
        &'a self,
        account: &'a AccountId,
        token: &'a RefreshToken,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            self.tokens
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(account.clone(), token.clone());
            Ok(())
        })
    }

    fn delete<'a>(&'a self, account: &'a AccountId) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            self.tokens
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(account);
            Ok(())
        })
    }
}

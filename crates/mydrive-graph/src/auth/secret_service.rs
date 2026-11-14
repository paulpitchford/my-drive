//! A [`TokenStore`] over the freedesktop Secret Service API on the session bus
//! (gnome-keyring, KWallet), using `zbus` (ADR-0010).
//!
//! Each account has one item in the default collection, found by the attributes
//! `application = mydrive` and `account = <account ID>`. Secrets travel to the service
//! with the `plain` session algorithm: the session bus is private to the user, and the
//! encrypted algorithm would need crypto crates outside the workspace stack. Accepted for
//! now and recorded as a follow-up.
//!
//! A locked collection or item is unlocked through the service's own prompt (J01 V4). A
//! dismissed prompt is [`StoreError::Dismissed`].

use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use futures_core::Stream as _;
use mydrive_core::ids::AccountId;
use serde::{Deserialize, Serialize};
use zbus::proxy::CacheProperties;
use zbus::{Connection, OwnedObjectPath, OwnedVariant, Type, Variant};

use super::error::StoreError;
use super::secret::RefreshToken;
use super::store::{BoxFuture, TokenStore};

const SERVICE: &str = "org.freedesktop.secrets";
const SERVICE_PATH: &str = "/org/freedesktop/secrets";
const SERVICE_IFACE: &str = "org.freedesktop.Secret.Service";
const COLLECTION_IFACE: &str = "org.freedesktop.Secret.Collection";
const ITEM_IFACE: &str = "org.freedesktop.Secret.Item";
const SESSION_IFACE: &str = "org.freedesktop.Secret.Session";
const PROMPT_IFACE: &str = "org.freedesktop.Secret.Prompt";
const DEFAULT_COLLECTION: &str = "/org/freedesktop/secrets/aliases/default";
const NO_PROMPT: &str = "/";

const ATTR_APPLICATION: &str = "application";
const APPLICATION: &str = "mydrive";
const ATTR_ACCOUNT: &str = "account";

/// How long we wait for the user to answer a keyring prompt.
const PROMPT_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// The Secret Service `Secret` struct, `(oayays)`. No `Debug`: it carries the secret.
#[derive(Serialize, Deserialize, Type)]
struct WireSecret {
    session: OwnedObjectPath,
    parameters: Vec<u8>,
    value: Vec<u8>,
    content_type: String,
}

/// A [`TokenStore`] backed by the Secret Service.
#[derive(Clone)]
pub struct SecretServiceStore {
    conn: Connection,
}

impl fmt::Debug for SecretServiceStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretServiceStore").finish_non_exhaustive()
    }
}

fn unavailable(e: &zbus::Error) -> StoreError {
    StoreError::Unavailable {
        reason: e.to_string(),
    }
}

fn failed(operation: &'static str) -> impl Fn(zbus::Error) -> StoreError {
    move |e| StoreError::Failed {
        operation,
        reason: e.to_string(),
    }
}

impl SecretServiceStore {
    /// Connects to the Secret Service on the session bus and checks that it answers.
    ///
    /// # Errors
    ///
    /// [`StoreError::Unavailable`], naming `mydrive doctor`, if there is no session bus or
    /// no Secret Service on it (ADR-0010 R7).
    pub async fn connect() -> Result<Self, StoreError> {
        let conn = Connection::session().await.map_err(|e| unavailable(&e))?;
        Self::with_connection(conn).await
    }

    /// Connects to the Secret Service on the bus at a D-Bus `address`, for example a
    /// private test bus.
    ///
    /// # Errors
    ///
    /// As [`SecretServiceStore::connect`].
    pub async fn connect_to(address: &str) -> Result<Self, StoreError> {
        let conn = zbus::connection::Builder::address(address)
            .build()
            .await
            .map_err(|e| unavailable(&e))?;
        Self::with_connection(conn).await
    }

    async fn with_connection(conn: Connection) -> Result<Self, StoreError> {
        let store = Self { conn };
        let session = store.open_session().await.map_err(|e| unavailable(&e))?;
        store.close_session(&session).await;
        Ok(store)
    }

    async fn call<B, R>(
        &self,
        path: &str,
        iface: &str,
        method: &str,
        body: &B,
    ) -> Result<R, zbus::Error>
    where
        B: Serialize + zbus::DynamicType,
        R: for<'d> zbus::zvariant::DynamicDeserialize<'d>,
    {
        let reply = self
            .conn
            .call_method(Some(SERVICE), path, Some(iface), method, body)
            .await?;
        reply.body().deserialize()
    }

    async fn open_session(&self) -> Result<OwnedObjectPath, zbus::Error> {
        let (_output, session): (OwnedVariant, OwnedObjectPath) = self
            .call(
                SERVICE_PATH,
                SERVICE_IFACE,
                "OpenSession",
                &("plain", Variant::from("")),
            )
            .await?;
        Ok(session)
    }

    async fn close_session(&self, session: &OwnedObjectPath) {
        let closed: Result<(), _> = self
            .call(session.as_str(), SESSION_IFACE, "Close", &())
            .await;
        if let Err(e) = closed {
            tracing::debug!(error = %e, "closing a Secret Service session failed");
        }
    }

    fn attributes(account: &AccountId) -> HashMap<&'static str, String> {
        HashMap::from([
            (ATTR_APPLICATION, APPLICATION.to_owned()),
            (ATTR_ACCOUNT, account.as_str().to_owned()),
        ])
    }

    /// The account's items, unlocked (prompting if needed).
    async fn find_items(
        &self,
        account: &AccountId,
        operation: &'static str,
    ) -> Result<Vec<OwnedObjectPath>, StoreError> {
        let (mut unlocked, locked): (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) = self
            .call(
                SERVICE_PATH,
                SERVICE_IFACE,
                "SearchItems",
                &Self::attributes(account),
            )
            .await
            .map_err(failed(operation))?;
        if !locked.is_empty() {
            self.unlock(&locked, operation).await?;
            unlocked.extend(locked);
        }
        Ok(unlocked)
    }

    async fn unlock(
        &self,
        objects: &[OwnedObjectPath],
        operation: &'static str,
    ) -> Result<(), StoreError> {
        let (_unlocked, prompt): (Vec<OwnedObjectPath>, OwnedObjectPath) = self
            .call(SERVICE_PATH, SERVICE_IFACE, "Unlock", &objects.to_vec())
            .await
            .map_err(failed(operation))?;
        self.run_prompt(&prompt, operation).await
    }

    /// Shows a prompt and waits for its `Completed` signal. `/` means no prompt is needed.
    async fn run_prompt(
        &self,
        prompt: &OwnedObjectPath,
        operation: &'static str,
    ) -> Result<(), StoreError> {
        if prompt.as_str() == NO_PROMPT {
            return Ok(());
        }
        let proxy: zbus::Proxy<'_> = zbus::proxy::Builder::new(&self.conn)
            .destination(SERVICE)
            .path(prompt.as_str())
            .interface(PROMPT_IFACE)
            .cache_properties(CacheProperties::No)
            .build()
            .await
            .map_err(failed(operation))?;
        // Subscribe before showing the prompt so the signal cannot be missed.
        let mut completed = proxy
            .receive_signal("Completed")
            .await
            .map_err(failed(operation))?;
        proxy
            .call_method("Prompt", &"")
            .await
            .map_err(failed(operation))?;
        let next = std::future::poll_fn(|cx| std::pin::Pin::new(&mut completed).poll_next(cx));
        let message = tokio::time::timeout(PROMPT_TIMEOUT, next)
            .await
            .map_err(|_| StoreError::Failed {
                operation,
                reason: "the keyring prompt was not answered".to_owned(),
            })?
            .ok_or_else(|| StoreError::Failed {
                operation,
                reason: "the Secret Service went away during a prompt".to_owned(),
            })?;
        let (dismissed, _result): (bool, OwnedVariant) =
            message.body().deserialize().map_err(failed(operation))?;
        if dismissed {
            return Err(StoreError::Dismissed { operation });
        }
        Ok(())
    }

    async fn load_inner(&self, account: &AccountId) -> Result<Option<RefreshToken>, StoreError> {
        const OP: &str = "reading a refresh token from the Secret Service";
        let items = self.find_items(account, OP).await?;
        let Some(item) = items.first() else {
            return Ok(None);
        };
        let session = self.open_session().await.map_err(failed(OP))?;
        let secret: Result<WireSecret, _> = self
            .call(item.as_str(), ITEM_IFACE, "GetSecret", &session)
            .await;
        self.close_session(&session).await;
        let secret = secret.map_err(failed(OP))?;
        let value = String::from_utf8(secret.value).map_err(|_| StoreError::Failed {
            operation: OP,
            reason: "the stored secret is not UTF-8".to_owned(),
        })?;
        Ok(Some(RefreshToken::new(value)))
    }

    async fn save_inner(
        &self,
        account: &AccountId,
        token: &RefreshToken,
    ) -> Result<(), StoreError> {
        const OP: &str = "storing a refresh token in the Secret Service";
        let collection = OwnedObjectPath::try_from(DEFAULT_COLLECTION).map_err(failed(OP))?;
        self.unlock(std::slice::from_ref(&collection), OP).await?;
        let label = format!("my-drive refresh token ({account})");
        let attributes = Self::attributes(account);
        let properties: HashMap<&str, Variant<'_>> = HashMap::from([
            (
                "org.freedesktop.Secret.Item.Label",
                Variant::from(label.as_str()),
            ),
            (
                "org.freedesktop.Secret.Item.Attributes",
                Variant::from(attributes),
            ),
        ]);
        let session = self.open_session().await.map_err(failed(OP))?;
        let secret = WireSecret {
            session: session.clone(),
            parameters: Vec::new(),
            value: token.expose_secret().as_bytes().to_vec(),
            content_type: "text/plain; charset=utf8".to_owned(),
        };
        // `replace = true`: an existing item with the same attributes is overwritten in one
        // call, so a failure leaves the previous token in place.
        let created: Result<(OwnedObjectPath, OwnedObjectPath), _> = self
            .call(
                DEFAULT_COLLECTION,
                COLLECTION_IFACE,
                "CreateItem",
                &(properties, secret, true),
            )
            .await;
        self.close_session(&session).await;
        let (_item, prompt) = created.map_err(failed(OP))?;
        self.run_prompt(&prompt, OP).await
    }

    async fn delete_inner(&self, account: &AccountId) -> Result<(), StoreError> {
        const OP: &str = "deleting a refresh token from the Secret Service";
        for item in self.find_items(account, OP).await? {
            let prompt: OwnedObjectPath = self
                .call(item.as_str(), ITEM_IFACE, "Delete", &())
                .await
                .map_err(failed(OP))?;
            self.run_prompt(&prompt, OP).await?;
        }
        Ok(())
    }
}

impl TokenStore for SecretServiceStore {
    fn load<'a>(
        &'a self,
        account: &'a AccountId,
    ) -> BoxFuture<'a, Result<Option<RefreshToken>, StoreError>> {
        Box::pin(self.load_inner(account))
    }

    fn save<'a>(
        &'a self,
        account: &'a AccountId,
        token: &'a RefreshToken,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(self.save_inner(account, token))
    }

    fn delete<'a>(&'a self, account: &'a AccountId) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(self.delete_inner(account))
    }
}

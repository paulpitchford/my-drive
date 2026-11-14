//! A fake freedesktop Secret Service for the auth tests, served on a private
//! peer-to-peer D-Bus connection (a Unix socket pair): no session bus, no real keyring.
//!
//! It models the default-collection alias, several collections, locked items, and prompts
//! that complete, are dismissed, or are never answered. Included by test files with
//! `#[path = "auth_fake_secret_service.rs"] mod fake_secret_service;`. Cargo also builds it
//! as an empty test binary; each including file uses only part of it.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use mydrive_core::ids::AccountId;
use serde::{Deserialize, Serialize};
use zbus::connection::Builder;
use zbus::object_server::SignalEmitter;
use zbus::{
    Connection, Guid, ObjectServer, OwnedObjectPath, OwnedVariant, Type, Variant, fdo, interface,
};

pub(crate) const LOGIN: &str = "/org/freedesktop/secrets/collection/login";
pub(crate) const OTHER: &str = "/org/freedesktop/secrets/collection/other";
pub(crate) const SESSION: &str = "/org/freedesktop/secrets/session/s1";

#[derive(Serialize, Deserialize, Type)]
struct FakeSecret {
    session: OwnedObjectPath,
    parameters: Vec<u8>,
    value: Vec<u8>,
    content_type: String,
}

pub(crate) struct StoredItem {
    pub(crate) path: String,
    pub(crate) collection: String,
    pub(crate) attributes: HashMap<String, String>,
    pub(crate) value: Vec<u8>,
}

/// What the fake does when a prompt is shown.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum PromptMode {
    /// The user accepts.
    #[default]
    Complete,
    /// The user dismisses it.
    Dismiss,
    /// The user never answers.
    Never,
}

#[derive(Default)]
pub(crate) struct Keyring {
    pub(crate) default_alias: Option<String>,
    pub(crate) fail_create: bool,
    /// CreateItem needs a prompt (for example a confirmation).
    pub(crate) prompt_on_create: bool,
    /// The keyring is locked: reading needs an unlock prompt.
    pub(crate) locked: bool,
    pub(crate) prompt_mode: PromptMode,
    pub(crate) items: Vec<StoredItem>,
    pub(crate) next: u32,
    pub(crate) create_calls: u32,
    pub(crate) prompts_shown: u32,
    pub(crate) dismiss_calls: u32,
}

pub(crate) type Shared = Arc<Mutex<Keyring>>;

pub(crate) fn lock(k: &Shared) -> std::sync::MutexGuard<'_, Keyring> {
    k.lock().unwrap_or_else(PoisonError::into_inner)
}

fn path(p: &str) -> fdo::Result<OwnedObjectPath> {
    OwnedObjectPath::try_from(p).map_err(|e| fdo::Error::Failed(e.to_string()))
}

fn failed(e: impl std::fmt::Display) -> fdo::Error {
    fdo::Error::Failed(e.to_string())
}

fn matches(item: &StoredItem, wanted: &HashMap<String, String>) -> bool {
    wanted
        .iter()
        .all(|(k, v)| item.attributes.get(k) == Some(v))
}

/// Creates or replaces an item and serves it. Returns its path.
async fn put_item(
    keyring: &Shared,
    server: &ObjectServer,
    collection: &str,
    attributes: HashMap<String, String>,
    value: Vec<u8>,
) -> fdo::Result<String> {
    let new_path = {
        let mut k = lock(keyring);
        if let Some(item) = k
            .items
            .iter_mut()
            .find(|i| i.collection == collection && i.attributes == attributes)
        {
            item.value = value;
            return Ok(item.path.clone());
        }
        k.next += 1;
        let p = format!("{collection}/{}", k.next);
        k.items.push(StoredItem {
            path: p.clone(),
            collection: collection.to_owned(),
            attributes,
            value,
        });
        p
    };
    server
        .at(
            new_path.as_str(),
            Item {
                keyring: keyring.clone(),
                path: new_path.clone(),
            },
        )
        .await
        .map_err(failed)?;
    Ok(new_path)
}

/// Registers a prompt that performs `action` when accepted. Returns its path.
async fn new_prompt(
    keyring: &Shared,
    server: &ObjectServer,
    action: Action,
) -> fdo::Result<String> {
    let p = {
        let mut k = lock(keyring);
        k.next += 1;
        format!("/org/freedesktop/secrets/prompt/p{}", k.next)
    };
    server
        .at(
            p.as_str(),
            Prompt {
                keyring: keyring.clone(),
                action: Mutex::new(Some(action)),
            },
        )
        .await
        .map_err(failed)?;
    Ok(p)
}

struct Service(Shared);

#[interface(name = "org.freedesktop.Secret.Service")]
impl Service {
    fn open_session(
        &self,
        _algorithm: &str,
        _input: OwnedVariant,
    ) -> fdo::Result<(OwnedVariant, OwnedObjectPath)> {
        let output = OwnedVariant::try_from(Variant::from("")).map_err(failed)?;
        Ok((output, path(SESSION)?))
    }

    fn read_alias(&self, name: &str) -> fdo::Result<OwnedObjectPath> {
        let alias = lock(&self.0).default_alias.clone();
        match (name, alias) {
            ("default", Some(p)) => path(&p),
            _ => path("/"),
        }
    }

    async fn unlock(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        objects: Vec<OwnedObjectPath>,
    ) -> fdo::Result<(Vec<OwnedObjectPath>, OwnedObjectPath)> {
        if !lock(&self.0).locked {
            return Ok((objects, path("/")?));
        }
        let p = new_prompt(&self.0, server, Action::Unlock).await?;
        Ok((Vec::new(), path(&p)?))
    }

    /// Searches every collection, as the real service does. The store must not use it.
    fn search_items(
        &self,
        attributes: HashMap<String, String>,
    ) -> fdo::Result<(Vec<OwnedObjectPath>, Vec<OwnedObjectPath>)> {
        let k = lock(&self.0);
        let found = k
            .items
            .iter()
            .filter(|i| matches(i, &attributes))
            .map(|i| path(&i.path))
            .collect::<fdo::Result<Vec<_>>>()?;
        Ok((found, Vec::new()))
    }
}

struct Collection {
    keyring: Shared,
    path: String,
}

#[interface(name = "org.freedesktop.Secret.Collection")]
impl Collection {
    fn search_items(
        &self,
        attributes: HashMap<String, String>,
    ) -> fdo::Result<Vec<OwnedObjectPath>> {
        let k = lock(&self.keyring);
        k.items
            .iter()
            .filter(|i| i.collection == self.path && matches(i, &attributes))
            .map(|i| path(&i.path))
            .collect()
    }

    async fn create_item(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        properties: HashMap<String, OwnedVariant>,
        secret: FakeSecret,
        _replace: bool,
    ) -> fdo::Result<(OwnedObjectPath, OwnedObjectPath)> {
        let attributes: HashMap<String, String> = properties
            .get("org.freedesktop.Secret.Item.Attributes")
            .ok_or_else(|| fdo::Error::InvalidArgs("no attributes".into()))?
            .try_clone()
            .map_err(failed)?
            .try_into()
            .map_err(|e: zbus::Error| failed(e))?;
        let (fail, needs_prompt) = {
            let mut k = lock(&self.keyring);
            k.create_calls += 1;
            (k.fail_create, k.prompt_on_create)
        };
        if fail {
            return Err(fdo::Error::Failed("cannot create the item".into()));
        }
        if needs_prompt {
            let p = new_prompt(
                &self.keyring,
                server,
                Action::Create {
                    collection: self.path.clone(),
                    attributes,
                    value: secret.value,
                },
            )
            .await?;
            return Ok((path("/")?, path(&p)?));
        }
        let item = put_item(&self.keyring, server, &self.path, attributes, secret.value).await?;
        Ok((path(&item)?, path("/")?))
    }
}

struct Item {
    keyring: Shared,
    path: String,
}

#[interface(name = "org.freedesktop.Secret.Item")]
impl Item {
    fn get_secret(&self, session: OwnedObjectPath) -> fdo::Result<FakeSecret> {
        let k = lock(&self.keyring);
        if k.locked {
            return Err(fdo::Error::Failed(
                "org.freedesktop.Secret.Error.IsLocked".into(),
            ));
        }
        let item = k
            .items
            .iter()
            .find(|i| i.path == self.path)
            .ok_or_else(|| fdo::Error::UnknownObject(self.path.clone()))?;
        Ok(FakeSecret {
            session,
            parameters: Vec::new(),
            value: item.value.clone(),
            content_type: "text/plain".into(),
        })
    }

    fn delete(&self) -> fdo::Result<OwnedObjectPath> {
        lock(&self.keyring).items.retain(|i| i.path != self.path);
        path("/")
    }
}

enum Action {
    Unlock,
    Create {
        collection: String,
        attributes: HashMap<String, String>,
        value: Vec<u8>,
    },
}

struct Prompt {
    keyring: Shared,
    action: Mutex<Option<Action>>,
}

#[interface(name = "org.freedesktop.Secret.Prompt")]
impl Prompt {
    async fn prompt(
        &self,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        #[zbus(object_server)] server: &ObjectServer,
        _window_id: &str,
    ) -> fdo::Result<()> {
        let mode = {
            let mut k = lock(&self.keyring);
            k.prompts_shown += 1;
            k.prompt_mode
        };
        match mode {
            PromptMode::Never => Ok(()),
            PromptMode::Dismiss => {
                self.action
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take();
                Self::completed(&emitter, true, Variant::from(""))
                    .await
                    .map_err(failed)
            }
            PromptMode::Complete => {
                let action = self
                    .action
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take();
                let result = match action {
                    Some(Action::Unlock) => {
                        lock(&self.keyring).locked = false;
                        String::new()
                    }
                    Some(Action::Create {
                        collection,
                        attributes,
                        value,
                    }) => put_item(&self.keyring, server, &collection, attributes, value).await?,
                    None => String::new(),
                };
                Self::completed(&emitter, false, Variant::from(result.as_str()))
                    .await
                    .map_err(failed)
            }
        }
    }

    async fn dismiss(&self, #[zbus(signal_emitter)] emitter: SignalEmitter<'_>) -> fdo::Result<()> {
        lock(&self.keyring).dismiss_calls += 1;
        self.action
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        Self::completed(&emitter, true, Variant::from(""))
            .await
            .map_err(failed)
    }

    #[zbus(signal)]
    async fn completed(
        emitter: &SignalEmitter<'_>,
        dismissed: bool,
        result: Variant<'_>,
    ) -> zbus::Result<()>;
}

struct Session;

#[interface(name = "org.freedesktop.Secret.Session")]
impl Session {
    fn close(&self) {}
}

/// Starts the fake service and returns a client connection to it, plus its state.
pub(crate) async fn fake_service(keyring: Keyring) -> Result<(Connection, Shared), zbus::Error> {
    let shared: Shared = Arc::new(Mutex::new(keyring));
    let (server_sock, client_sock) = std::os::unix::net::UnixStream::pair()?;
    let guid = Guid::generate();
    let server = Builder::unix_stream(server_sock)
        .server(guid)
        .p2p()
        .serve_at("/org/freedesktop/secrets", Service(shared.clone()))
        .serve_at(
            LOGIN,
            Collection {
                keyring: shared.clone(),
                path: LOGIN.into(),
            },
        )
        .serve_at(
            OTHER,
            Collection {
                keyring: shared.clone(),
                path: OTHER.into(),
            },
        )
        .serve_at(SESSION, Session)
        .build();
    let client = Builder::unix_stream(client_sock).p2p().build();
    let (server, client) = tokio::join!(server, client);
    let server = server?;
    let client = client?;
    // Pre-existing items need objects too.
    let paths: Vec<String> = lock(&shared).items.iter().map(|i| i.path.clone()).collect();
    for p in paths {
        server
            .object_server()
            .at(
                p.as_str(),
                Item {
                    keyring: shared.clone(),
                    path: p.clone(),
                },
            )
            .await?;
    }
    // Keep the server connection alive for the test's lifetime.
    std::mem::forget(server);
    Ok((client, shared))
}

pub(crate) fn account() -> AccountId {
    AccountId::new("acct-1").unwrap_or_else(|e| panic!("{e}"))
}

pub(crate) fn our_attributes() -> HashMap<String, String> {
    HashMap::from([
        ("xdg:schema".into(), "mydrive.RefreshToken".into()),
        ("application".into(), "mydrive".into()),
        ("account".into(), "acct-1".into()),
    ])
}

pub(crate) fn our_item(value: &str) -> StoredItem {
    StoredItem {
        path: format!("{LOGIN}/1"),
        collection: LOGIN.into(),
        attributes: our_attributes(),
        value: value.as_bytes().to_vec(),
    }
}

/// Waits (bounded) for the fake to reach a state; the fake runs on other tasks.
pub(crate) async fn eventually(keyring: &Shared, cond: impl Fn(&Keyring) -> bool) -> bool {
    for _ in 0..200 {
        if cond(&lock(keyring)) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    cond(&lock(keyring))
}

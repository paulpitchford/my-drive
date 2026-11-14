//! `SecretServiceStore` against a fake Secret Service on a private peer-to-peer D-Bus
//! connection (a Unix socket pair). No session bus and no real keyring is involved.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use mydrive_core::ids::AccountId;
use mydrive_graph::auth::{RefreshToken, SecretServiceStore, StoreError, TokenStore};
use serde::{Deserialize, Serialize};
use zbus::connection::Builder;
use zbus::{Connection, Guid, ObjectServer, OwnedObjectPath, OwnedVariant, Type, fdo, interface};

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

const LOGIN: &str = "/org/freedesktop/secrets/collection/login";
const OTHER: &str = "/org/freedesktop/secrets/collection/other";
const SESSION: &str = "/org/freedesktop/secrets/session/s1";

#[derive(Serialize, Deserialize, Type)]
struct FakeSecret {
    session: OwnedObjectPath,
    parameters: Vec<u8>,
    value: Vec<u8>,
    content_type: String,
}

struct StoredItem {
    path: String,
    collection: String,
    attributes: HashMap<String, String>,
    value: Vec<u8>,
}

#[derive(Default)]
struct Keyring {
    default_alias: Option<String>,
    fail_create: bool,
    items: Vec<StoredItem>,
    next: u32,
}

type Shared = Arc<Mutex<Keyring>>;

fn lock(k: &Shared) -> std::sync::MutexGuard<'_, Keyring> {
    k.lock().unwrap_or_else(PoisonError::into_inner)
}

fn path(p: &str) -> fdo::Result<OwnedObjectPath> {
    OwnedObjectPath::try_from(p).map_err(|e| fdo::Error::Failed(e.to_string()))
}

fn matches(item: &StoredItem, wanted: &HashMap<String, String>) -> bool {
    wanted
        .iter()
        .all(|(k, v)| item.attributes.get(k) == Some(v))
}

struct Service(Shared);

#[interface(name = "org.freedesktop.Secret.Service")]
impl Service {
    fn open_session(
        &self,
        _algorithm: &str,
        _input: OwnedVariant,
    ) -> fdo::Result<(OwnedVariant, OwnedObjectPath)> {
        let output = OwnedVariant::try_from(zbus::Variant::from(""))
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        Ok((output, path(SESSION)?))
    }

    fn read_alias(&self, name: &str) -> fdo::Result<OwnedObjectPath> {
        let alias = lock(&self.0).default_alias.clone();
        match (name, alias) {
            ("default", Some(p)) => path(&p),
            _ => path("/"),
        }
    }

    fn unlock(
        &self,
        objects: Vec<OwnedObjectPath>,
    ) -> fdo::Result<(Vec<OwnedObjectPath>, OwnedObjectPath)> {
        Ok((objects, path("/")?))
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
        replace: bool,
    ) -> fdo::Result<(OwnedObjectPath, OwnedObjectPath)> {
        let attributes: HashMap<String, String> = properties
            .get("org.freedesktop.Secret.Item.Attributes")
            .ok_or_else(|| fdo::Error::InvalidArgs("no attributes".into()))?
            .try_clone()
            .map_err(|e| fdo::Error::Failed(e.to_string()))?
            .try_into()
            .map_err(|e: zbus::Error| fdo::Error::Failed(e.to_string()))?;
        let new_path = {
            let mut k = lock(&self.keyring);
            if k.fail_create {
                return Err(fdo::Error::Failed("cannot create the item".into()));
            }
            let existing = k
                .items
                .iter_mut()
                .find(|i| i.collection == self.path && i.attributes == attributes);
            match (existing, replace) {
                (Some(item), true) => {
                    item.value = secret.value;
                    return Ok((path(&item.path)?, path("/")?));
                }
                _ => {
                    k.next += 1;
                    let p = format!("{}/{}", self.path, k.next);
                    k.items.push(StoredItem {
                        path: p.clone(),
                        collection: self.path.clone(),
                        attributes,
                        value: secret.value,
                    });
                    p
                }
            }
        };
        server
            .at(
                new_path.as_str(),
                Item {
                    keyring: self.keyring.clone(),
                    path: new_path.clone(),
                },
            )
            .await
            .map_err(|e| fdo::Error::Failed(e.to_string()))?;
        Ok((path(&new_path)?, path("/")?))
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

struct Session;

#[interface(name = "org.freedesktop.Secret.Session")]
impl Session {
    fn close(&self) {}
}

/// Starts the fake service and returns a client connection to it, plus its state.
async fn fake_service(keyring: Keyring) -> Result<(Connection, Shared), zbus::Error> {
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

fn account() -> AccountId {
    AccountId::new("acct-1").unwrap_or_else(|e| panic!("{e}"))
}

fn our_attributes() -> HashMap<String, String> {
    HashMap::from([
        ("xdg:schema".into(), "mydrive.RefreshToken".into()),
        ("application".into(), "mydrive".into()),
        ("account".into(), "acct-1".into()),
    ])
}

/// ADR-0010 R6, review item 11: tokens round-trip through the default collection with a
/// schema attribute, and saving replaces the account's one entry.
#[tokio::test]
async fn round_trip_in_default_collection_with_schema() -> TestResult {
    let (conn, keyring) = fake_service(Keyring {
        default_alias: Some(LOGIN.into()),
        ..Keyring::default()
    })
    .await?;
    let store = SecretServiceStore::with_connection(conn).await?;
    assert!(store.load(&account()).await?.is_none());
    store
        .save(&account(), &RefreshToken::new("dummy-1"))
        .await?;
    store
        .save(&account(), &RefreshToken::new("dummy-2"))
        .await?;
    assert_eq!(
        store
            .load(&account())
            .await?
            .as_ref()
            .map(RefreshToken::expose_secret),
        Some("dummy-2")
    );
    {
        let k = lock(&keyring);
        assert_eq!(k.items.len(), 1, "one entry per account");
        assert_eq!(k.items[0].collection, LOGIN);
        assert_eq!(k.items[0].attributes, our_attributes());
    }
    store.delete(&account()).await?;
    assert!(store.load(&account()).await?.is_none());
    Ok(())
}

/// Review item 11: an item outside the default collection, or one from another
/// application without our schema attribute, is never read as our token.
#[tokio::test]
async fn ignores_other_collections_and_other_schemas() -> TestResult {
    let mut other_app = our_attributes();
    other_app.remove("xdg:schema");
    let (conn, _) = fake_service(Keyring {
        default_alias: Some(LOGIN.into()),
        items: vec![
            StoredItem {
                path: format!("{OTHER}/1"),
                collection: OTHER.into(),
                attributes: our_attributes(),
                value: b"from-other-collection".to_vec(),
            },
            StoredItem {
                path: format!("{LOGIN}/2"),
                collection: LOGIN.into(),
                attributes: other_app,
                value: b"from-other-application".to_vec(),
            },
        ],
        next: 10,
        ..Keyring::default()
    })
    .await?;
    let store = SecretServiceStore::with_connection(conn).await?;
    assert!(store.load(&account()).await?.is_none());
    Ok(())
}

/// Review item 11, ADR-0010 R7: no default collection is an error naming `mydrive doctor`.
#[tokio::test]
async fn no_default_collection_names_doctor() -> TestResult {
    let (conn, _) = fake_service(Keyring::default()).await?;
    let store = SecretServiceStore::with_connection(conn).await?;
    let e = store
        .save(&account(), &RefreshToken::new("dummy"))
        .await
        .err()
        .ok_or("save must fail")?;
    assert!(matches!(e, StoreError::Unavailable { .. }), "{e:?}");
    assert!(e.to_string().contains("mydrive doctor"), "{e}");
    let e = store.load(&account()).await.err().ok_or("load must fail")?;
    assert!(e.to_string().contains("mydrive doctor"), "{e}");
    Ok(())
}

/// Review item 11, ADR-0010 R7: when the service cannot create the item, the error names
/// `mydrive doctor`.
#[tokio::test]
async fn create_failure_names_doctor() -> TestResult {
    let (conn, _) = fake_service(Keyring {
        default_alias: Some(LOGIN.into()),
        fail_create: true,
        ..Keyring::default()
    })
    .await?;
    let store = SecretServiceStore::with_connection(conn).await?;
    let e = store
        .save(&account(), &RefreshToken::new("dummy"))
        .await
        .err()
        .ok_or("save must fail")?;
    let shown = e.to_string();
    assert!(shown.contains("mydrive doctor"), "{shown}");
    assert!(!shown.contains("dummy"), "{shown}");
    Ok(())
}

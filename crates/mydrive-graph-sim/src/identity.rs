//! Identity: `/me`, `/me/drive` and `/organization`, and the registry of accounts and
//! tenants behind them, with the control API that builds it.
//!
//! The registry ([`Directory`]) also holds the fake authority's state (`authority`) and
//! the discovery state (`discovery`). It sits behind its own lock in `Shared`, beside the
//! drives' `State`; the two locks are never held at the same time (see
//! `docs/identity.md`). Documented in `docs/identity.md`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

use axum::Router;
use axum::extract::State as AxState;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use axum::routing::get;
use mydrive_core::ids::DriveId;
use serde_json::{Value, json};

use crate::authority::{AuthorityState, IdTokenIssue, OldRefreshToken, ReusedCode};
use crate::discovery::{DiscoveryState, LibraryAccess, ShareWithoutPrefer};
use crate::json::drive_json;
use crate::model::DriveType;
use crate::server::{Shared, graph_error, ok_json};
use crate::{GraphSim, SimError};

/// The tenant ID the identity platform puts in the `tid` claim of every personal Microsoft
/// account: a documented public constant, not any one account's data (ID token claims
/// reference). See `docs/identity.md`.
pub const CONSUMER_TENANT_ID: &str = "9188040d-6c67-4c5b-b112-36a304b66dad";

/// Personal Microsoft account or work or school account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccountKind {
    /// A personal Microsoft account; its own drive is a personal OneDrive.
    Personal,
    /// A work or school account in a tenant; its own drive is a OneDrive for Business.
    Work,
}

/// A tenant's ID: the GUID that appears as the `tid` claim and as `/organization`'s `id`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TenantId(String);

impl TenantId {
    /// The GUID.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TenantId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One simulated account, as the control API returns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// The object ID: the `oid` claim and `/me`'s `id` for a work account. Unique per
    /// simulator; the account is identified by it in the control API.
    pub object_id: String,
    /// The sign-in name: the `preferred_username` claim and `/me`'s `userPrincipalName`.
    pub user_name: String,
    /// The display name: the `name` claim and `/me`'s `displayName`.
    pub display_name: String,
    /// Personal or work.
    pub kind: AccountKind,
    /// The tenant of a work account; `None` for a personal account.
    pub tenant: Option<TenantId>,
    /// The account's own drive, as `/me/drive` returns it.
    pub drive: DriveId,
}

impl Account {
    /// The `tid` claim: the tenant's ID, or [`CONSUMER_TENANT_ID`] for a personal account.
    #[must_use]
    pub fn tenant_claim(&self) -> &str {
        self.tenant
            .as_ref()
            .map_or(CONSUMER_TENANT_ID, TenantId::as_str)
    }

    /// The `id` `/me` returns. For a work account the object ID; for a personal account
    /// the 16-hex-digit form, which the simulator makes equal to the personal drive's ID
    /// (unverified, see `docs/identity.md`).
    #[must_use]
    pub fn graph_user_id(&self) -> String {
        match self.kind {
            AccountKind::Work => self.object_id.clone(),
            AccountKind::Personal => self.drive.as_str().to_owned(),
        }
    }
}

/// What `/organization` answers for a personal account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PersonalOrganization {
    /// `403 accessDenied`. Unverified; the default, since a client that reads a 403 as a
    /// policy block gets it wrong.
    Forbidden,
    /// `200` with an empty `value` list.
    Empty,
}

/// Behaviour switches of the identity side, for the whole simulator. Set them with
/// [`GraphSim::set_identity_options`].
///
/// As for `DriveOptions`: each default is what Microsoft documents, or where that is
/// unknown, the case that is harder for a client. `docs/identity.md` gives the sources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityOptions {
    /// Whether a refresh token keeps working once it has been used. Default
    /// [`OldRefreshToken::Refused`], as WP-023 sets it; the identity platform documents
    /// that the old token is not revoked on use (see `docs/identity.md`).
    pub old_refresh_token: OldRefreshToken,
    /// When the token endpoint returns an ID token. Default
    /// [`IdTokenIssue::WithOpenidScope`], as documented.
    pub id_token: IdTokenIssue,
    /// What `/organization` answers for a personal account. Unverified; default
    /// [`PersonalOrganization::Forbidden`].
    pub personal_organization: PersonalOrganization,
    /// What resolving a sharing link without a `Prefer` header does for an account
    /// without access. Unverified; default [`ShareWithoutPrefer::Refused`].
    pub share_without_prefer: ShareWithoutPrefer,
    /// What redeeming a code a second time does. Unverified for the identity platform;
    /// default [`ReusedCode::RevokeIssuedTokens`], RFC 6749's SHOULD and the harder case.
    pub reused_code: ReusedCode,
}

impl Default for IdentityOptions {
    fn default() -> Self {
        Self {
            old_refresh_token: OldRefreshToken::Refused,
            id_token: IdTokenIssue::WithOpenidScope,
            personal_organization: PersonalOrganization::Forbidden,
            share_without_prefer: ShareWithoutPrefer::Refused,
            reused_code: ReusedCode::RevokeIssuedTokens,
        }
    }
}

/// Why an identity control call failed.
#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    /// Creating the account's or library's drive failed.
    #[error(transparent)]
    Sim(#[from] SimError),
    /// The tenant was not created by this simulator.
    #[error("tenant {tenant} does not exist in this simulator")]
    UnknownTenant {
        /// The tenant asked for.
        tenant: TenantId,
    },
    /// The account was not created by this simulator.
    #[error("account {object_id} does not exist in this simulator")]
    UnknownAccount {
        /// The object ID asked for.
        object_id: String,
    },
    /// The site was not created by this simulator.
    #[error("site {site} does not exist in this simulator")]
    UnknownSite {
        /// The site ID asked for.
        site: String,
    },
}

/// One tenant in the registry.
#[derive(Debug)]
pub(crate) struct TenantRecord {
    pub(crate) display_name: String,
    pub(crate) domain: String,
    /// Object IDs of the tenant's accounts, in creation order.
    accounts: Vec<String>,
}

/// The registry of tenants and accounts, and the state of the authority and discovery.
/// Held by `Shared` behind its own lock.
///
/// Its `Debug` never prints a token: bound tokens are shown as a count.
pub(crate) struct Directory {
    pub(crate) options: IdentityOptions,
    /// Distinguishes this simulator's tokens, codes and IDs from another's.
    pub(crate) instance: String,
    /// The authority's origin, the base of the ID tokens' `iss` claim.
    pub(crate) issuer_origin: String,
    next_number: u64,
    pub(crate) tenants: BTreeMap<TenantId, TenantRecord>,
    accounts: BTreeMap<String, Account>,
    /// The tenant each tenant drive (business drive or library) belongs to.
    drive_tenants: HashMap<DriveId, TenantId>,
    /// Drives each account may reach besides its own: granted, or reached through a
    /// redeemed sharing link.
    access: HashMap<String, HashSet<DriveId>>,
    /// Bearer tokens bound to an account: issued by the authority, or bound by a test.
    token_accounts: HashMap<String, String>,
    pub(crate) authority: AuthorityState,
    pub(crate) discovery: DiscoveryState,
}

impl fmt::Debug for Directory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Directory")
            .field("options", &self.options)
            .field("instance", &self.instance)
            .field("tenants", &self.tenants)
            .field("accounts", &self.accounts)
            .field("bound_tokens", &self.token_accounts.len())
            .field("authority", &self.authority)
            .field("discovery", &self.discovery)
            .finish_non_exhaustive()
    }
}

impl Directory {
    pub(crate) fn new(instance: String, issuer_origin: String) -> Self {
        Self {
            options: IdentityOptions::default(),
            instance,
            issuer_origin,
            next_number: 0,
            tenants: BTreeMap::new(),
            accounts: BTreeMap::new(),
            drive_tenants: HashMap::new(),
            access: HashMap::new(),
            token_accounts: HashMap::new(),
            authority: AuthorityState::default(),
            discovery: DiscoveryState::default(),
        }
    }

    /// A number unique within this simulator, for IDs.
    pub(crate) fn next_number(&mut self) -> u64 {
        self.next_number += 1;
        self.next_number
    }

    /// A GUID-shaped ID unique within this simulator. `kind` is one hex digit that tells
    /// tenants, accounts and sites apart when reading a log.
    pub(crate) fn new_guid(&mut self, kind: u8) -> String {
        let n = self.next_number();
        format!("{kind:x}0000000-5000-4000-8000-{n:012x}")
    }

    pub(crate) fn account(&self, object_id: &str) -> Option<&Account> {
        self.accounts.get(object_id)
    }

    fn known_account(&self, account: &Account) -> Result<&Account, IdentityError> {
        self.accounts
            .get(&account.object_id)
            .ok_or_else(|| IdentityError::UnknownAccount {
                object_id: account.object_id.clone(),
            })
    }

    /// The account a bearer token is bound to, if any.
    pub(crate) fn account_for_token(&self, token: &str) -> Option<&Account> {
        self.token_accounts
            .get(token)
            .and_then(|oid| self.accounts.get(oid))
    }

    pub(crate) fn bind_token(&mut self, token: String, object_id: String) {
        self.token_accounts.insert(token, object_id);
    }

    /// Whether the account may reach the drive: its own, or one it was given.
    pub(crate) fn can_reach(&self, account: &Account, drive: &DriveId) -> bool {
        account.drive == *drive
            || self
                .access
                .get(&account.object_id)
                .is_some_and(|drives| drives.contains(drive))
    }

    pub(crate) fn grant(&mut self, object_id: &str, drive: DriveId) {
        self.access
            .entry(object_id.to_owned())
            .or_default()
            .insert(drive);
    }

    pub(crate) fn tenant_of_drive(&self, drive: &DriveId) -> Option<&TenantId> {
        self.drive_tenants.get(drive)
    }

    pub(crate) fn add_tenant_drive(&mut self, tenant: TenantId, drive: DriveId) {
        self.drive_tenants.insert(drive, tenant);
    }

    fn insert_account(&mut self, account: Account) {
        if let Some(tenant) = &account.tenant {
            if let Some(record) = self.tenants.get_mut(tenant) {
                record.accounts.push(account.object_id.clone());
            }
            self.drive_tenants
                .insert(account.drive.clone(), tenant.clone());
        }
        self.accounts.insert(account.object_id.clone(), account);
    }
}

/// The bearer token of a request, if it has one.
pub(crate) fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            let (scheme, token) = value.split_once(' ')?;
            scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
        })
}

/// The identity routes of the Graph origin.
pub(crate) fn routes() -> Router<Arc<Shared>> {
    Router::new()
        .route("/v1.0/me", get(get_me))
        .route("/v1.0/me/drive", get(get_my_drive))
        .route("/v1.0/organization", get(get_organization))
}

/// The answer for a token that the bearer check accepted but no account owns: the fixed
/// per-instance token, or one added with `add_token` and not bound.
fn unbound() -> Response {
    graph_error(
        StatusCode::FORBIDDEN,
        "accessDenied",
        "Simulator: this bearer token is not bound to an account (see GraphSim::bind_token).",
    )
}

/// The account behind the request's token. Takes and releases the directory lock.
fn caller(shared: &Shared, headers: &HeaderMap) -> Option<Account> {
    let token = bearer(headers)?;
    shared.directory().account_for_token(token).cloned()
}

fn user_json(account: &Account) -> Value {
    json!({
        "id": account.graph_user_id(),
        "displayName": account.display_name,
        "userPrincipalName": account.user_name,
        "mail": account.user_name,
    })
}

// ---- handlers --------------------------------------------------------------------------

async fn get_me(AxState(shared): AxState<Arc<Shared>>, headers: HeaderMap) -> Response {
    match caller(&shared, &headers) {
        Some(account) => ok_json(user_json(&account)),
        None => unbound(),
    }
}

async fn get_my_drive(AxState(shared): AxState<Arc<Shared>>, headers: HeaderMap) -> Response {
    let Some(account) = caller(&shared, &headers) else {
        return unbound();
    };
    // The account's own drive follows the library policy as `/drives/{id}` does.
    if let Some(LibraryAccess::Blocked { code }) =
        shared.directory().discovery.access_of(&account.drive)
    {
        return crate::discovery::blocked(&code);
    }
    // The directory lock is released; now the drives' lock.
    let state = shared.lock();
    match state.drives.get(&account.drive) {
        Some(drive) => ok_json(drive_json(drive)),
        None => crate::server::not_found(),
    }
}

async fn get_organization(AxState(shared): AxState<Arc<Shared>>, headers: HeaderMap) -> Response {
    let Some(account) = caller(&shared, &headers) else {
        return unbound();
    };
    let directory = shared.directory();
    let Some(tenant) = account.tenant.as_ref() else {
        return match directory.options.personal_organization {
            PersonalOrganization::Forbidden => graph_error(
                StatusCode::FORBIDDEN,
                "accessDenied",
                "Simulator: a personal account has no organization.",
            ),
            PersonalOrganization::Empty => ok_json(json!({ "value": [] })),
        };
    };
    let Some(record) = directory.tenants.get(tenant) else {
        return crate::server::not_found();
    };
    ok_json(json!({
        "value": [{
            "id": tenant.as_str(),
            "displayName": record.display_name,
            "verifiedDomains": [{
                "name": record.domain,
                "isDefault": true,
                "isInitial": true,
                "type": "Managed",
            }],
        }]
    }))
}

// ---- control API -----------------------------------------------------------------------

impl GraphSim {
    /// The identity switches in force.
    #[must_use]
    pub fn identity_options(&self) -> IdentityOptions {
        self.shared.directory().options.clone()
    }

    /// Replaces the identity switches. Takes effect from the next request.
    pub fn set_identity_options(&self, options: IdentityOptions) {
        self.shared.directory().options = options;
    }

    /// Adds a tenant (an organisation) and returns its ID. Use obviously fake names, for
    /// example `"Contoso"` and `"contoso.example.invalid"`.
    pub fn add_tenant(&self, display_name: &str, domain: &str) -> TenantId {
        let mut directory = self.shared.directory();
        let id = TenantId(directory.new_guid(1));
        directory.tenants.insert(
            id.clone(),
            TenantRecord {
                display_name: display_name.to_owned(),
                domain: domain.to_owned(),
                accounts: Vec::new(),
            },
        );
        id
    }

    /// Adds a personal Microsoft account with its own personal drive.
    ///
    /// # Errors
    ///
    /// [`IdentityError::Sim`] only if the drive cannot be created, which is a bug.
    pub fn add_personal_account(
        &self,
        user_name: &str,
        display_name: &str,
    ) -> Result<Account, IdentityError> {
        // The drives' lock, then (released) the directory's.
        let drive = self.add_drive(DriveType::Personal)?;
        let hex = drive.as_str().to_ascii_lowercase();
        // Personal object IDs are zero-padded GUIDs around the 16-hex-digit user ID
        // (unverified, docs/identity.md).
        let object_id = format!(
            "00000000-0000-0000-{}-{}",
            hex.get(..4).unwrap_or("0000"),
            hex.get(4..).unwrap_or("000000000000")
        );
        let account = Account {
            object_id,
            user_name: user_name.to_owned(),
            display_name: display_name.to_owned(),
            kind: AccountKind::Personal,
            tenant: None,
            drive,
        };
        self.shared.directory().insert_account(account.clone());
        Ok(account)
    }

    /// Adds a work or school account in `tenant`, with its own OneDrive for Business.
    ///
    /// # Errors
    ///
    /// [`IdentityError::UnknownTenant`] if the tenant does not exist here.
    pub fn add_work_account(
        &self,
        tenant: &TenantId,
        user_name: &str,
        display_name: &str,
    ) -> Result<Account, IdentityError> {
        if !self.shared.directory().tenants.contains_key(tenant) {
            return Err(IdentityError::UnknownTenant {
                tenant: tenant.clone(),
            });
        }
        let drive = self.add_drive(DriveType::Business)?;
        let mut directory = self.shared.directory();
        let account = Account {
            object_id: directory.new_guid(2),
            user_name: user_name.to_owned(),
            display_name: display_name.to_owned(),
            kind: AccountKind::Work,
            tenant: Some(tenant.clone()),
            drive,
        };
        directory.insert_account(account.clone());
        Ok(account)
    }

    /// Makes `token` (for example [`GraphSim::token`], or one added with
    /// [`GraphSim::add_token`]) act as `account`'s: `/me` answers for it, and drive
    /// requests are limited to what the account may reach. A token issued by the
    /// authority is bound to its account already.
    ///
    /// # Errors
    ///
    /// [`IdentityError::UnknownAccount`] if the account does not exist here.
    pub fn bind_token(&self, token: &str, account: &Account) -> Result<(), IdentityError> {
        let mut directory = self.shared.directory();
        let object_id = directory.known_account(account)?.object_id.clone();
        directory.bind_token(token.to_owned(), object_id);
        Ok(())
    }

    /// Lets `account` reach `drive`, as membership of a site or a share would.
    ///
    /// # Errors
    ///
    /// [`IdentityError::UnknownAccount`] if the account does not exist here.
    pub fn grant_access(&self, account: &Account, drive: &DriveId) -> Result<(), IdentityError> {
        let mut directory = self.shared.directory();
        let object_id = directory.known_account(account)?.object_id.clone();
        directory.grant(&object_id, drive.clone());
        Ok(())
    }

    /// Whether `account` may reach `drive` now: its own drive, a granted one, or one
    /// reached through a redeemed sharing link.
    #[must_use]
    pub fn can_reach(&self, account: &Account, drive: &DriveId) -> bool {
        self.shared.directory().can_reach(account, drive)
    }

    /// The tenant a business drive or library belongs to; `None` for a personal drive or
    /// a drive added without a tenant.
    #[must_use]
    pub fn tenant_of_drive(&self, drive: &DriveId) -> Option<TenantId> {
        self.shared.directory().tenant_of_drive(drive).cloned()
    }

    /// The tenant's accounts, in creation order.
    #[must_use]
    pub fn tenant_accounts(&self, tenant: &TenantId) -> Vec<Account> {
        let directory = self.shared.directory();
        directory
            .tenants
            .get(tenant)
            .map_or_else(Vec::new, |record| {
                record
                    .accounts
                    .iter()
                    .filter_map(|oid| directory.account(oid).cloned())
                    .collect()
            })
    }

    /// The tenant's drives: its accounts' own drives and its libraries, in ID order.
    #[must_use]
    pub fn tenant_drives(&self, tenant: &TenantId) -> Vec<DriveId> {
        let directory = self.shared.directory();
        let drives: BTreeSet<DriveId> = directory
            .drive_tenants
            .iter()
            .filter(|(_, t)| *t == tenant)
            .map(|(d, _)| d.clone())
            .collect();
        drives.into_iter().collect()
    }
}

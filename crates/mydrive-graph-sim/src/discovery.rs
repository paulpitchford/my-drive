//! Discovery: resolving sharing links, the site and library details that name a mount,
//! and libraries that are blocked by policy or read-only.
//!
//! The blocked and read-only checks are a layer (`library_policy`) that `server.rs` puts in
//! front of every Graph route, inside the bearer-token check, so write handlers need not
//! know about them. Both signals are provisional guesses until gate G2. Documented in
//! `docs/identity.md`.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Request, State as AxState};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use axum::routing::get;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use mydrive_core::ids::{DriveId, ItemId};
use serde_json::json;

use crate::content::download_url;
use crate::identity::{IdentityError, TenantId, bearer};
use crate::json::{View, item_json, percent_encode};
use crate::model::DriveType;
use crate::server::{Shared, graph_error, not_found, ok_json};
use crate::{GraphSim, SimError};

/// The Graph error code a blocked or read-only library answers with unless the test sets
/// another. Provisional (gate G2).
pub const DEFAULT_POLICY_CODE: &str = "accessDenied";

/// The message of a blocked library's `403`: the text SharePoint shows under "Block
/// access" for unmanaged devices. Whether Graph returns it is unverified (gate G2).
pub const BLOCKED_MESSAGE: &str = "Access Denied. Due to organization policy, you can't access \
     this resource from this untrusted device.";

/// What a library allows. Both refusals are **provisional guesses** until gate G2 reports
/// what Graph really returns; see `docs/identity.md`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum LibraryAccess {
    /// Everything the routes allow.
    #[default]
    Normal,
    /// Every request for the drive answers `403` with this error code, as an
    /// unmanaged-device policy might.
    Blocked {
        /// The Graph error `code` of the `403`.
        code: String,
    },
    /// Reads work; any request whose method writes (anything but `GET` and `HEAD`)
    /// answers `403` with this error code.
    ReadOnly {
        /// The Graph error `code` of the `403`.
        code: String,
    },
}

impl LibraryAccess {
    /// Blocked with [`DEFAULT_POLICY_CODE`].
    #[must_use]
    pub fn blocked() -> Self {
        Self::Blocked {
            code: DEFAULT_POLICY_CODE.to_owned(),
        }
    }

    /// Read-only with [`DEFAULT_POLICY_CODE`].
    #[must_use]
    pub fn read_only() -> Self {
        Self::ReadOnly {
            code: DEFAULT_POLICY_CODE.to_owned(),
        }
    }
}

/// What resolving a sharing link with no `Prefer` header does for an account that cannot
/// reach the drive yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShareWithoutPrefer {
    /// `403 accessDenied`. Unverified; the default, the harder case.
    Refused,
    /// The item, without durable access, as with `redeemSharingLinkIfNecessary`.
    Peek,
}

/// A SharePoint site's ID, `<host>,<site collection GUID>,<web GUID>`, the composite form
/// Graph uses.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SiteId(String);

impl SiteId {
    /// The composite ID.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SiteId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug)]
struct SiteRecord {
    tenant: TenantId,
    display_name: String,
    /// The URL name, `<display name without spaces>`.
    name: String,
    host: String,
}

impl SiteRecord {
    fn web_url(&self) -> String {
        format!("https://{}/sites/{}", self.host, percent_encode(&self.name))
    }
}

/// The discovery part of the identity registry.
#[derive(Debug, Default)]
pub(crate) struct DiscoveryState {
    sites: BTreeMap<SiteId, SiteRecord>,
    /// The site each library belongs to.
    libraries: HashMap<DriveId, SiteId>,
    access: HashMap<DriveId, LibraryAccess>,
    /// Sharing URLs and the item each opens.
    links: HashMap<String, (DriveId, ItemId)>,
    next_link: u64,
}

impl DiscoveryState {
    /// What a drive allows, if it is not [`LibraryAccess::Normal`].
    pub(crate) fn access_of(&self, drive: &DriveId) -> Option<LibraryAccess> {
        self.access.get(drive).cloned()
    }

    /// The code of a block on any library of `site`, if one is blocked.
    fn site_block(&self, site: &SiteId) -> Option<String> {
        self.libraries
            .iter()
            .filter(|(_, s)| *s == site)
            .find_map(|(drive, _)| match self.access.get(drive) {
                Some(LibraryAccess::Blocked { code }) => Some(code.clone()),
                _ => None,
            })
    }
}

/// The discovery routes of the Graph origin.
pub(crate) fn routes() -> Router<Arc<Shared>> {
    Router::new()
        .route("/v1.0/shares/{share}/driveItem", get(resolve_share))
        .route("/v1.0/sites/{site}", get(get_site))
        .route("/v1.0/drives/{drive}/list", get(get_list))
}

/// Encodes a sharing URL for `/shares/{id}` as the shares documentation says: base64,
/// then unpadded base64url, prefixed `u!`.
#[must_use]
pub fn encode_sharing_url(url: &str) -> String {
    format!("u!{}", URL_SAFE_NO_PAD.encode(url.as_bytes()))
}

fn decode_sharing_url(encoded: &str) -> Option<String> {
    let body = encoded.strip_prefix("u!")?.trim_end_matches('=');
    let bytes = URL_SAFE_NO_PAD.decode(body.as_bytes()).ok()?;
    String::from_utf8(bytes).ok()
}

/// Decodes `%XX` escapes; `None` if an escape is malformed or the result is not UTF-8.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The drive a Graph path addresses, for `/v1.0/drives/{drive}` and everything below it.
fn drive_in_path(path: &str) -> Option<DriveId> {
    let rest = path.strip_prefix("/v1.0/drives/")?;
    let segment = rest.split('/').next()?;
    DriveId::new(percent_decode(segment)?).ok()
}

/// Whether a request's method can change something. Only `GET` and `HEAD` cannot.
pub(crate) fn method_writes(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD)
}

fn access_denied() -> Response {
    graph_error(
        StatusCode::FORBIDDEN,
        "accessDenied",
        "Access denied: the caller cannot reach this drive.",
    )
}

pub(crate) fn blocked(code: &str) -> Response {
    graph_error(StatusCode::FORBIDDEN, code, BLOCKED_MESSAGE)
}

fn read_only(code: &str) -> Response {
    graph_error(
        StatusCode::FORBIDDEN,
        code,
        "Simulator: this library is read-only; writes are refused (provisional, gate G2).",
    )
}

/// The layer in front of every Graph route: a drive the caller's account cannot reach
/// answers `403 accessDenied`; a blocked library answers `403` with its code on every
/// route; a read-only library refuses every method that writes. It runs inside the bearer
/// check, so only authenticated requests reach it, and takes only the directory lock
/// (then, separately, the drives' lock to tell an unreachable drive from a missing one).
pub(crate) async fn library_policy(
    AxState(shared): AxState<Arc<Shared>>,
    req: Request,
    next: Next,
) -> Response {
    let Some(drive) = drive_in_path(req.uri().path()) else {
        return next.run(req).await;
    };
    let (unreachable, access) = {
        let directory = shared.directory();
        let account = bearer(req.headers()).and_then(|t| directory.account_for_token(t));
        (
            account.is_some_and(|a| !directory.can_reach(a, &drive)),
            directory.discovery.access.get(&drive).cloned(),
        )
    };
    // A drive that does not exist is left to the route, which answers 404.
    if unreachable && shared.lock().drives.contains_key(&drive) {
        return access_denied();
    }
    match access {
        Some(LibraryAccess::Blocked { code }) => blocked(&code),
        Some(LibraryAccess::ReadOnly { code }) if method_writes(req.method()) => read_only(&code),
        _ => next.run(req).await,
    }
}

/// What a `Prefer` header asks of `/shares`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Redeem {
    Durable,
    ForThisRequest,
    NotAsked,
}

fn prefer(headers: &HeaderMap) -> Redeem {
    let mut out = Redeem::NotAsked;
    for value in headers.get_all("prefer") {
        for pref in value.to_str().unwrap_or("").split(',').map(str::trim) {
            if pref.eq_ignore_ascii_case("redeemSharingLink") {
                out = Redeem::Durable;
            } else if pref.eq_ignore_ascii_case("redeemSharingLinkIfNecessary")
                && out == Redeem::NotAsked
            {
                out = Redeem::ForThisRequest;
            }
        }
    }
    out
}

// ---- handlers --------------------------------------------------------------------------

async fn resolve_share(
    AxState(shared): AxState<Arc<Shared>>,
    Path(share): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Some(url) = decode_sharing_url(&share) else {
        return not_found();
    };
    let Some((drive, item)) = shared.directory().discovery.links.get(&url).cloned() else {
        return not_found();
    };
    let alive = shared
        .lock()
        .drives
        .get(&drive)
        .is_some_and(|d| d.live(&item).is_ok());
    if !alive {
        return not_found();
    }
    let site = {
        let mut directory = shared.directory();
        if let Some(LibraryAccess::Blocked { code }) = directory.discovery.access.get(&drive) {
            return blocked(code);
        }
        let account = bearer(&headers)
            .and_then(|t| directory.account_for_token(t))
            .cloned();
        if let Some(account) = account {
            let link_tenant = directory.tenant_of_drive(&drive);
            if link_tenant.is_some() && link_tenant != account.tenant.as_ref() {
                return access_denied();
            }
            match prefer(&headers) {
                Redeem::Durable => directory.grant(&account.object_id, drive.clone()),
                Redeem::ForThisRequest => {}
                Redeem::NotAsked => {
                    if !directory.can_reach(&account, &drive)
                        && directory.options.share_without_prefer == ShareWithoutPrefer::Refused
                    {
                        return access_denied();
                    }
                }
            }
        }
        directory.discovery.libraries.get(&drive).cloned()
    };
    let mut state = shared.lock();
    let download = download_url(&shared, &mut state, &drive, &item);
    let Some(d) = state.drives.get(&drive) else {
        return not_found();
    };
    let Ok(i) = d.live(&item) else {
        return not_found();
    };
    let mut value = item_json(d, i, View::Item, download.as_deref());
    if let Some(site) = site {
        value["parentReference"]["siteId"] = json!(site.as_str());
    }
    ok_json(value)
}

async fn get_site(
    AxState(shared): AxState<Arc<Shared>>,
    Path(site): Path<String>,
    headers: HeaderMap,
) -> Response {
    let directory = shared.directory();
    let id = SiteId(site);
    let Some(record) = directory.discovery.sites.get(&id) else {
        return not_found();
    };
    // An account outside the site's tenant (a personal one included) cannot read it.
    // Unverified: the status for another tenant's site.
    let account = bearer(&headers).and_then(|t| directory.account_for_token(t));
    if account.is_some_and(|a| a.tenant.as_ref() != Some(&record.tenant)) {
        return access_denied();
    }
    // A blocked library models a per-site policy (unmanaged-device controls can be per
    // site), so its site answers as the library does. Unverified (gate G2).
    if let Some(code) = directory.discovery.site_block(&id) {
        return blocked(&code);
    }
    ok_json(json!({
        "id": id.as_str(),
        "name": record.name,
        "displayName": record.display_name,
        "webUrl": record.web_url(),
        "siteCollection": { "hostname": record.host },
    }))
}

async fn get_list(AxState(shared): AxState<Arc<Shared>>, Path(drive): Path<String>) -> Response {
    let Ok(drive) = DriveId::new(drive) else {
        return not_found();
    };
    let directory = shared.directory();
    let Some(site) = directory.discovery.libraries.get(&drive) else {
        return not_found();
    };
    let Some(record) = directory.discovery.sites.get(site) else {
        return not_found();
    };
    ok_json(json!({
        "id": format!("{}-list", drive.as_str()),
        "name": "Shared Documents",
        "displayName": "Documents",
        "webUrl": format!("{}/Shared%20Documents", record.web_url()),
        "list": { "template": "documentLibrary", "hidden": false },
        "parentReference": { "siteId": site.as_str() },
    }))
}

// ---- control API -----------------------------------------------------------------------

impl GraphSim {
    /// Adds a SharePoint site to `tenant` and returns its ID. Its host is
    /// `<first label of the tenant's domain>.sharepoint.example.invalid`.
    ///
    /// # Errors
    ///
    /// [`IdentityError::UnknownTenant`] if the tenant does not exist here.
    pub fn add_site(&self, tenant: &TenantId, display_name: &str) -> Result<SiteId, IdentityError> {
        let mut directory = self.shared.directory();
        let Some(record) = directory.tenants.get(tenant) else {
            return Err(IdentityError::UnknownTenant {
                tenant: tenant.clone(),
            });
        };
        let label = record
            .domain
            .split('.')
            .next()
            .unwrap_or("tenant")
            .to_owned();
        let host = format!("{label}.sharepoint.example.invalid");
        let id = SiteId(format!(
            "{host},{},{}",
            directory.new_guid(3),
            directory.new_guid(4)
        ));
        directory.discovery.sites.insert(
            id.clone(),
            SiteRecord {
                tenant: tenant.clone(),
                display_name: display_name.to_owned(),
                name: display_name.replace(' ', ""),
                host,
            },
        );
        Ok(id)
    }

    /// Adds a document library to `site` and returns its drive ID. The library belongs to
    /// the site's tenant; no account can reach it until granted access or a sharing link
    /// to it is redeemed. Its name is "Documents" (see `docs/identity.md`).
    ///
    /// # Errors
    ///
    /// [`IdentityError::UnknownSite`] if the site does not exist here.
    pub fn add_library(&self, site: &SiteId) -> Result<DriveId, IdentityError> {
        let Some(tenant) = self
            .shared
            .directory()
            .discovery
            .sites
            .get(site)
            .map(|s| s.tenant.clone())
        else {
            return Err(IdentityError::UnknownSite {
                site: site.as_str().to_owned(),
            });
        };
        // The drives' lock, released, then the directory's.
        let drive = self.add_drive(DriveType::DocumentLibrary)?;
        let mut directory = self.shared.directory();
        directory
            .discovery
            .libraries
            .insert(drive.clone(), site.clone());
        directory.add_tenant_drive(tenant, drive.clone());
        Ok(drive)
    }

    /// The site a library belongs to.
    #[must_use]
    pub fn site_of_library(&self, drive: &DriveId) -> Option<SiteId> {
        self.shared
            .directory()
            .discovery
            .libraries
            .get(drive)
            .cloned()
    }

    /// Sets what a drive allows: blocked, read-only or normal. Meant for libraries; any
    /// drive accepts it. Provisional until gate G2.
    ///
    /// # Errors
    ///
    /// [`SimError::UnknownDrive`] if the drive does not exist.
    pub fn set_library_access(
        &self,
        drive: &DriveId,
        access: LibraryAccess,
    ) -> Result<(), SimError> {
        self.shared.lock().drive(drive)?;
        let mut directory = self.shared.directory();
        if access == LibraryAccess::Normal {
            directory.discovery.access.remove(drive);
        } else {
            directory.discovery.access.insert(drive.clone(), access);
        }
        Ok(())
    }

    /// What the drive allows now.
    #[must_use]
    pub fn library_access(&self, drive: &DriveId) -> LibraryAccess {
        self.shared
            .directory()
            .discovery
            .access
            .get(drive)
            .cloned()
            .unwrap_or_default()
    }

    /// Creates a sharing link to a live item and returns its URL, for example
    /// `https://contoso.sharepoint.example.invalid/:f:/s/Finance/Sim1`. Resolve it with
    /// `GET /shares/{encode_sharing_url(url)}/driveItem`.
    ///
    /// # Errors
    ///
    /// [`SimError::UnknownDrive`] or [`SimError::UnknownItem`] inside
    /// [`IdentityError::Sim`].
    pub fn add_sharing_link(
        &self,
        drive: &DriveId,
        item: &ItemId,
    ) -> Result<String, IdentityError> {
        let is_folder = self.shared.lock().drive(drive)?.live(item)?.is_folder();
        let mut directory = self.shared.directory();
        directory.discovery.next_link += 1;
        let n = directory.discovery.next_link;
        let kind = if is_folder { "f" } else { "u" };
        let url = match directory.discovery.libraries.get(drive) {
            Some(site) => match directory.discovery.sites.get(site) {
                Some(record) => {
                    format!("https://{}/:{kind}:/s/{}/Sim{n}", record.host, record.name)
                }
                None => format!("https://share.example.invalid/:{kind}:/Sim{n}"),
            },
            None => format!("https://share.example.invalid/:{kind}:/Sim{n}"),
        };
        directory
            .discovery
            .links
            .insert(url.clone(), (drive.clone(), item.clone()));
        Ok(url)
    }
}

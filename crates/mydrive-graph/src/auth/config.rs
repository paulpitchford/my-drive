//! Sign-in parameters: client ID, authority and scopes (ADR-0010).

use std::fmt;

use url::Url;

use super::error::ConfigError;

/// The compiled-in client ID.
///
/// A placeholder (the nil UUID) until the owner registers the app (ADR-0010, open question
/// Q22). Sign-in against Microsoft with this value will fail; it exists so that the override
/// path from `config.toml` (ADR-0010 R4) can be built and tested now.
pub const DEFAULT_CLIENT_ID: &str = "00000000-0000-0000-0000-000000000000";

/// The documented default authority: Entra's `common` endpoint, which accepts personal and
/// work or school accounts (ADR-0010). Tests inject their own.
pub const DEFAULT_AUTHORITY: &str = "https://login.microsoftonline.com/common/";

/// The redirect URI of the headless flow in F01 (Entra's native-client page), which shows
/// the code in the address bar of any device. The paste flow takes its redirect URI as a
/// parameter, so a caller may use a loopback URI instead (as J14 describes).
pub const NATIVE_CLIENT_REDIRECT_URI: &str =
    "https://login.microsoftonline.com/common/oauth2/nativeclient";

/// Scopes of the initial sign-in, exactly as ADR-0010 R5 lists them.
pub const INITIAL_SCOPES: [&str; 3] = ["Files.ReadWrite.All", "offline_access", "User.Read"];

/// The scope requested incrementally, only from the site-search flow (ADR-0010 R5, ADR-0015).
pub const SITE_SEARCH_SCOPE: &str = "Sites.Read.All";

/// The public client ID of the app registration. Not a secret: a public client has none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientId(String);

impl ClientId {
    /// The client ID from `config.toml` when it is set, else [`DEFAULT_CLIENT_ID`]
    /// (ADR-0010 R4). A value that is empty after trimming counts as not set.
    #[must_use]
    pub fn from_config(configured: Option<&str>) -> Self {
        match configured.map(str::trim) {
            Some(id) if !id.is_empty() => Self(id.to_owned()),
            _ => Self(DEFAULT_CLIENT_ID.to_owned()),
        }
    }

    /// The client ID as sent in requests.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ClientId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A set of OAuth scopes, kept in the order given and sent space-separated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scopes(Vec<String>);

impl Scopes {
    /// The scopes of the initial sign-in (ADR-0010 R5).
    #[must_use]
    pub fn initial_sign_in() -> Self {
        Self(INITIAL_SCOPES.iter().map(|s| (*s).to_owned()).collect())
    }

    /// The initial scopes plus [`SITE_SEARCH_SCOPE`], for the site-search flow only.
    #[must_use]
    pub fn with_site_search() -> Self {
        let mut s = Self::initial_sign_in();
        s.0.push(SITE_SEARCH_SCOPE.to_owned());
        s
    }

    /// Any other set of scopes.
    #[must_use]
    pub fn new<I, S>(scopes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self(scopes.into_iter().map(Into::into).collect())
    }

    /// The `scope` parameter value: the scopes separated by single spaces.
    #[must_use]
    pub fn to_param(&self) -> String {
        self.0.join(" ")
    }
}

/// Where and as whom to sign in.
#[derive(Clone, Debug)]
pub struct AuthConfig {
    client_id: ClientId,
    authority: Url,
}

impl AuthConfig {
    /// A configuration for `client_id` against [`DEFAULT_AUTHORITY`].
    ///
    /// # Errors
    ///
    /// Never in practice; [`ConfigError`] only if the compiled-in authority failed to parse.
    pub fn new(client_id: ClientId) -> Result<Self, ConfigError> {
        Self::with_authority(client_id, DEFAULT_AUTHORITY)
    }

    /// A configuration for `client_id` against another authority base URL, for example a
    /// tenant-specific authority or a fake one in tests. A missing trailing slash is added,
    /// so that the endpoint paths are joined below it.
    ///
    /// # Errors
    ///
    /// [`ConfigError::InvalidAuthority`] if `authority` is not an absolute `http` or
    /// `https` URL without query or fragment.
    pub fn with_authority(client_id: ClientId, authority: &str) -> Result<Self, ConfigError> {
        let invalid = |reason: &'static str| ConfigError::InvalidAuthority {
            authority: authority.to_owned(),
            reason,
        };
        let mut url = Url::parse(authority).map_err(|_| invalid("not an absolute URL"))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(invalid("scheme is not http or https"));
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(invalid("has a query or fragment"));
        }
        if !url.path().ends_with('/') {
            let path = format!("{}/", url.path());
            url.set_path(&path);
        }
        Ok(Self {
            client_id,
            authority: url,
        })
    }

    /// The client ID.
    #[must_use]
    pub fn client_id(&self) -> &ClientId {
        &self.client_id
    }

    /// The authority base URL, ending in `/`.
    #[must_use]
    pub fn authority(&self) -> &Url {
        &self.authority
    }

    /// The v2.0 authorisation endpoint below the authority.
    ///
    /// # Errors
    ///
    /// Never in practice: the relative path is a constant.
    pub fn authorize_endpoint(&self) -> Result<Url, ConfigError> {
        self.endpoint("oauth2/v2.0/authorize")
    }

    /// The v2.0 token endpoint below the authority.
    ///
    /// # Errors
    ///
    /// Never in practice: the relative path is a constant.
    pub fn token_endpoint(&self) -> Result<Url, ConfigError> {
        self.endpoint("oauth2/v2.0/token")
    }

    fn endpoint(&self, path: &'static str) -> Result<Url, ConfigError> {
        self.authority
            .join(path)
            .map_err(|_| ConfigError::InvalidAuthority {
                authority: self.authority.to_string(),
                reason: "cannot join the endpoint path",
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR-0010 R4: the configured client ID wins; otherwise the compiled-in default.
    #[test]
    fn client_id_from_config_else_default() {
        assert_eq!(
            ClientId::from_config(Some("11111111-2222-3333-4444-555555555555")).as_str(),
            "11111111-2222-3333-4444-555555555555"
        );
        assert_eq!(ClientId::from_config(None).as_str(), DEFAULT_CLIENT_ID);
        assert_eq!(
            ClientId::from_config(Some("  ")).as_str(),
            DEFAULT_CLIENT_ID
        );
    }

    /// ADR-0010 R5: the initial scopes are exactly these three, and `Sites.Read.All` is not
    /// among them.
    #[test]
    fn initial_scopes_are_exact() {
        let s = Scopes::initial_sign_in();
        assert_eq!(s.to_param(), "Files.ReadWrite.All offline_access User.Read");
        assert!(!s.to_param().contains(SITE_SEARCH_SCOPE));
        assert!(
            Scopes::with_site_search()
                .to_param()
                .ends_with(" Sites.Read.All")
        );
    }

    #[test]
    fn endpoints_are_below_the_authority() -> Result<(), ConfigError> {
        let c = AuthConfig::new(ClientId::from_config(None))?;
        assert_eq!(
            c.token_endpoint()?.as_str(),
            "https://login.microsoftonline.com/common/oauth2/v2.0/token"
        );
        let c = AuthConfig::with_authority(ClientId::from_config(None), "http://127.0.0.1:9/t")?;
        assert_eq!(
            c.authorize_endpoint()?.as_str(),
            "http://127.0.0.1:9/t/oauth2/v2.0/authorize"
        );
        Ok(())
    }

    #[test]
    fn rejects_bad_authorities() {
        let id = ClientId::from_config(None);
        for bad in ["not a url", "ftp://x/", "https://x/?a=b", "https://x/#f"] {
            assert!(
                AuthConfig::with_authority(id.clone(), bad).is_err(),
                "{bad}"
            );
        }
    }
}

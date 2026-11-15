//! AC5 of WP-023: the existing loopback and paste sign-in flows complete against the
//! simulator's fake authority, and the tokens they get work on the simulated Graph through
//! the existing client. Nothing here contacts Microsoft or a real keyring.

// `cfg(test)` tells clippy the helpers are test code, so they may unwrap.
#[cfg(test)]
mod sim_identity_sign_in {
    use std::sync::Arc;
    use std::time::Duration;

    use mydrive_core::ids::AccountId;
    use mydrive_graph::GraphClient;
    use mydrive_graph::auth::{
        AuthConfig, ClientId, LoopbackSignIn, MemoryTokenStore, PasteSignIn,
        RefreshingTokenProvider, Scopes, SignInError, TokenClient, TokenSet,
    };
    use mydrive_graph::client::UserAgent;
    use mydrive_graph::model::DriveType;
    use mydrive_graph_sim::{Account, GraphSim, StatusCode};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpStream;
    use url::Url;

    const CLIENT_ID: &str = "11111111-2222-3333-4444-555555555555";
    const PASTE_REDIRECT: &str = "https://auth.example.invalid/nativeclient";

    fn config(sim: &GraphSim) -> AuthConfig {
        AuthConfig::with_authority(ClientId::from_config(Some(CLIENT_ID)), &sim.authority_url())
            .unwrap()
    }

    /// Plays the browser at the authorise step: opens the URL and returns where the
    /// authority redirected to.
    async fn browser_authorize(url: &Url) -> Url {
        let response = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
            .get(url.as_str())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);
        let location = response.headers()["location"].to_str().unwrap();
        Url::parse(location).unwrap()
    }

    /// Delivers a redirect to the loopback listener, as the browser does.
    async fn browser_deliver(port: u16, redirect: &Url) -> String {
        let target = match redirect.query() {
            Some(q) => format!("{}?{q}", redirect.path()),
            None => redirect.path().to_owned(),
        };
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let request = format!("GET {target} HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
    }

    /// Checks the tokens work on the simulated Graph through the existing client, for the
    /// account's own drive.
    async fn use_tokens(
        sim: &GraphSim,
        account: &Account,
        tokens: TokenSet,
        client: TokenClient,
    ) -> mydrive_graph::model::Drive {
        let provider = RefreshingTokenProvider::signed_in(
            AccountId::new("sim-account").unwrap(),
            tokens,
            Scopes::initial_sign_in(),
            client,
            Arc::new(MemoryTokenStore::new()),
        )
        .await
        .unwrap();
        let graph =
            GraphClient::new(sim.base_url(), Arc::new(provider), UserAgent::default()).unwrap();
        graph.get_drive(&account.drive).await.unwrap()
    }

    /// AC5 (loopback), ADR-0010 R1: `LoopbackSignIn` completes against the simulator's
    /// authority (which checks the verifier against the S256 challenge), and the access
    /// token reads the personal account's own drive through `GraphClient`. Its refresh
    /// token rolls; the used one is then refused as needing sign-in.
    #[tokio::test]
    async fn loopback_sign_in_completes_against_the_simulator() {
        let sim = GraphSim::start().await.unwrap();
        let account = sim
            .add_personal_account("sam@example.invalid", "Sam Example")
            .unwrap();
        sim.sign_in_as(&account).unwrap();
        let config = config(&sim);
        let client = TokenClient::new(&config).unwrap();

        let sign_in = LoopbackSignIn::start(&config, Scopes::initial_sign_in())
            .await
            .unwrap();
        let port = sign_in.local_addr().port();
        let redirect = browser_authorize(sign_in.authorization_url()).await;
        assert_eq!(redirect.port(), Some(port));
        let (tokens, page) = tokio::join!(
            sign_in.complete(&client, Duration::from_secs(30)),
            browser_deliver(port, &redirect)
        );
        assert!(page.starts_with("HTTP/1.1 200"), "{page}");
        let tokens = tokens.unwrap();
        // The client's initial scopes do not include `openid`, so no ID token.
        assert!(tokens.id_token.is_none());
        let old = tokens.refresh_token.clone().unwrap();

        let drive = use_tokens(&sim, &account, tokens, client.clone()).await;
        assert_eq!(drive.id, account.drive);
        assert_eq!(drive.drive_type, Some(DriveType::Personal));

        let rolled = client
            .refresh(&old, &Scopes::initial_sign_in())
            .await
            .unwrap();
        let new = rolled.refresh_token.unwrap();
        assert_ne!(new.expose_secret(), old.expose_secret());
        let refused = client
            .refresh(&old, &Scopes::initial_sign_in())
            .await
            .err()
            .unwrap();
        assert!(refused.needs_sign_in(), "{refused}");
    }

    /// AC5 (paste), ADR-0010 R10: `PasteSignIn` completes from the address the simulator's
    /// authority redirected to, for a work account, and the access token reads its
    /// business drive through `GraphClient`. A pasted address with another `state` is
    /// refused first.
    #[tokio::test]
    async fn paste_sign_in_completes_against_the_simulator() {
        let sim = GraphSim::start().await.unwrap();
        let tenant = sim.add_tenant("Contoso", "contoso.example.invalid");
        let account = sim
            .add_work_account(&tenant, "priya@contoso.example.invalid", "Priya Example")
            .unwrap();
        sim.sign_in_as(&account).unwrap();
        let config = config(&sim);
        let client = TokenClient::new(&config).unwrap();

        let mut sign_in =
            PasteSignIn::start(&config, PASTE_REDIRECT, Scopes::initial_sign_in()).unwrap();
        let redirect = browser_authorize(sign_in.authorization_url().unwrap()).await;
        assert!(redirect.as_str().starts_with(PASTE_REDIRECT), "{redirect}");

        let tokens = sign_in.complete(redirect.as_str(), &client).await.unwrap();
        assert!(!sign_in.is_pending());
        let drive = use_tokens(&sim, &account, tokens, client).await;
        assert_eq!(drive.id, account.drive);
        assert_eq!(drive.drive_type, Some(DriveType::Business));

        // A second attempt: an address with a forged state is refused and cancels it.
        let mut second =
            PasteSignIn::start(&config, PASTE_REDIRECT, Scopes::initial_sign_in()).unwrap();
        let mut forged = browser_authorize(second.authorization_url().unwrap()).await;
        let code = forged
            .query_pairs()
            .find(|(k, _)| k == "code")
            .map(|(_, v)| v.into_owned())
            .unwrap();
        forged.set_query(Some(&format!("code={code}&state=forged")));
        let err = second
            .complete(forged.as_str(), &TokenClient::new(&config).unwrap())
            .await
            .err()
            .unwrap();
        assert!(matches!(err, SignInError::StateMismatch), "{err}");
    }
}

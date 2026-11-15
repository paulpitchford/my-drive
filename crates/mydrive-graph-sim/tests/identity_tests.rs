//! Self-tests for the fake authority and the identity endpoints (WP-023), using a plain
//! HTTP client. Each acceptance criterion has a test named `acN_...`.

// `cfg(test)` tells clippy the helpers are test code, so they may unwrap.
#[cfg(test)]
mod identity_tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use mydrive_graph_sim::{
        AccountKind, CONSUMER_TENANT_ID, DriveType, GraphSim, IdTokenIssue, IdentityOptions,
        OldRefreshToken, Origin, PersonalOrganization, REFRESH_TOKEN_LIFETIME_SECS, StatusCode,
    };
    use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, LOCATION};
    use reqwest::redirect::Policy;
    use serde_json::Value;
    use sha2::{Digest as _, Sha256};

    const CLIENT_ID: &str = "11111111-2222-3333-4444-555555555555";
    const REDIRECT: &str = "http://localhost:53682";
    const SCOPES: &str = "openid Files.ReadWrite.All offline_access User.Read";
    const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";

    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .redirect(Policy::none())
            .build()
            .unwrap()
    }

    fn encode(text: &str) -> String {
        let mut out = String::new();
        for byte in text.bytes() {
            if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                out.push(char::from(byte));
            } else {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
        out
    }

    fn decode(text: &str) -> String {
        let bytes = text.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' {
                out.push(u8::from_str_radix(&text[i + 1..i + 3], 16).unwrap());
                i += 3;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        String::from_utf8(out).unwrap()
    }

    fn form(pairs: &[(&str, &str)]) -> String {
        pairs
            .iter()
            .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
            .collect::<Vec<_>>()
            .join("&")
    }

    fn challenge(verifier: &str) -> String {
        URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
    }

    /// The value of `name` in a URL's query.
    fn param(url: &str, name: &str) -> Option<String> {
        let query = url.split_once('?')?.1;
        query.split('&').find_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            (k == name).then(|| decode(v))
        })
    }

    /// Runs the authorise step as a browser would and returns the redirect's `Location`.
    async fn authorize(sim: &GraphSim, extra: &[(&str, &str)]) -> (StatusCode, String) {
        let ch = challenge(VERIFIER);
        let mut pairs = vec![
            ("client_id", CLIENT_ID),
            ("response_type", "code"),
            ("redirect_uri", REDIRECT),
            ("response_mode", "query"),
            ("scope", SCOPES),
            ("state", "state-1"),
            ("code_challenge", ch.as_str()),
            ("code_challenge_method", "S256"),
        ];
        for (name, value) in extra {
            match pairs.iter_mut().find(|(n, _)| n == name) {
                Some(pair) => pair.1 = value,
                None => pairs.push((name, value)),
            }
        }
        let url = format!(
            "{}oauth2/v2.0/authorize?{}",
            sim.authority_url(),
            form(&pairs)
        );
        let response = client().get(url).send().await.unwrap();
        let location = response
            .headers()
            .get(LOCATION)
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default();
        (response.status(), location)
    }

    async fn code(sim: &GraphSim) -> String {
        let (status, location) = authorize(sim, &[]).await;
        assert_eq!(status, StatusCode::FOUND);
        assert!(location.starts_with(&format!("{REDIRECT}?")), "{location}");
        assert_eq!(param(&location, "state").as_deref(), Some("state-1"));
        param(&location, "code").unwrap()
    }

    async fn post_token(sim: &GraphSim, pairs: &[(&str, &str)]) -> (StatusCode, Value) {
        let response = client()
            .post(format!("{}oauth2/v2.0/token", sim.authority_url()))
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(form(pairs))
            .send()
            .await
            .unwrap();
        let status = response.status();
        (
            status,
            serde_json::from_slice(&response.bytes().await.unwrap()).unwrap(),
        )
    }

    async fn redeem(sim: &GraphSim, code: &str, verifier: &str) -> (StatusCode, Value) {
        post_token(
            sim,
            &[
                ("client_id", CLIENT_ID),
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", REDIRECT),
                ("code_verifier", verifier),
                ("scope", SCOPES),
            ],
        )
        .await
    }

    async fn refresh(sim: &GraphSim, token: &str) -> (StatusCode, Value) {
        post_token(
            sim,
            &[
                ("client_id", CLIENT_ID),
                ("grant_type", "refresh_token"),
                ("refresh_token", token),
                ("scope", SCOPES),
            ],
        )
        .await
    }

    fn claims(id_token: &str) -> (Value, Value) {
        let parts: Vec<&str> = id_token.split('.').collect();
        assert_eq!(parts.len(), 3, "a JWT has three segments");
        assert_eq!(parts[2], "", "an unsecured JWT has an empty signature");
        let header = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
        let body = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        (header, body)
    }

    async fn graph_get(sim: &GraphSim, path: &str, token: &str) -> (StatusCode, Value) {
        let response = client()
            .get(format!("{}{path}", sim.base_url()))
            .bearer_auth(token)
            .send()
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.bytes().await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    /// A work account in a fake tenant, chosen as the one signing in.
    async fn work_sim() -> (GraphSim, mydrive_graph_sim::Account) {
        let sim = GraphSim::start().await.unwrap();
        let tenant = sim.add_tenant("Contoso", "contoso.example.invalid");
        let account = sim
            .add_work_account(&tenant, "priya@contoso.example.invalid", "Priya Example")
            .unwrap();
        sim.sign_in_as(&account).unwrap();
        (sim, account)
    }

    /// AC1, ADR-0010 R1 (the authority's side): a code redeemed with the wrong verifier is
    /// refused with `invalid_grant`; one redeemed with the verifier matching its S256
    /// challenge yields tokens whose ID token carries the account's claims.
    #[tokio::test]
    async fn ac1_verifier_is_checked_and_id_token_carries_claims() {
        let (sim, account) = work_sim().await;

        let first = code(&sim).await;
        let (status, body) =
            redeem(&sim, &first, "a-wrong-verifier-of-sufficient-length-000000").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "invalid_grant");
        // A code is redeemable once, even after a failed attempt.
        let (status, _) = redeem(&sim, &first, VERIFIER).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let second = code(&sim).await;
        let (status, body) = redeem(&sim, &second, VERIFIER).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["token_type"], "Bearer");
        assert!(
            body["access_token"]
                .as_str()
                .unwrap()
                .starts_with("sim-at-")
        );
        assert!(
            body["refresh_token"]
                .as_str()
                .unwrap()
                .starts_with("sim-rt-")
        );
        assert_eq!(body["expires_in"], 3600);
        let (header, claims) = claims(body["id_token"].as_str().unwrap());
        assert_eq!(header["alg"], "none");
        assert_eq!(claims["tid"], account.tenant.as_ref().unwrap().as_str());
        assert_eq!(claims["oid"], account.object_id.as_str());
        assert_eq!(
            claims["preferred_username"],
            "priya@contoso.example.invalid"
        );
        assert_eq!(claims["name"], "Priya Example");
        assert_eq!(claims["aud"], CLIENT_ID);
        assert_eq!(claims["ver"], "2.0");
        // The redeemed code is gone.
        let (status, _) = redeem(&sim, &second, VERIFIER).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    /// AC1: a missing verifier, another redirect URI or another client is refused too.
    #[tokio::test]
    async fn ac1_code_is_bound_to_client_redirect_and_challenge() {
        let (sim, _) = work_sim().await;
        let c = code(&sim).await;
        let (status, body) = post_token(
            &sim,
            &[
                ("client_id", CLIENT_ID),
                ("grant_type", "authorization_code"),
                ("code", &c),
                ("redirect_uri", REDIRECT),
            ],
        )
        .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::BAD_REQUEST, Some("invalid_grant"))
        );

        let c = code(&sim).await;
        let (status, _) = post_token(
            &sim,
            &[
                ("client_id", CLIENT_ID),
                ("grant_type", "authorization_code"),
                ("code", &c),
                ("redirect_uri", "http://localhost:1"),
                ("code_verifier", VERIFIER),
            ],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let c = code(&sim).await;
        let (status, _) = post_token(
            &sim,
            &[
                ("client_id", "99999999-0000-0000-0000-000000000000"),
                ("grant_type", "authorization_code"),
                ("code", &c),
                ("redirect_uri", REDIRECT),
                ("code_verifier", VERIFIER),
            ],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    /// AC1: a code expires after ten minutes of the simulator's clock.
    #[tokio::test]
    async fn ac1_code_expires() {
        let (sim, _) = work_sim().await;
        let c = code(&sim).await;
        sim.advance_clock(601);
        let (status, body) = redeem(&sim, &c, VERIFIER).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::BAD_REQUEST, Some("invalid_grant"))
        );
    }

    /// AC1: a personal account's ID token carries the documented consumer tenant ID.
    #[tokio::test]
    async fn ac1_personal_id_token_has_consumer_tenant() {
        let sim = GraphSim::start().await.unwrap();
        let account = sim
            .add_personal_account("sam@example.invalid", "Sam Example")
            .unwrap();
        sim.sign_in_as(&account).unwrap();
        let c = code(&sim).await;
        let (status, body) = redeem(&sim, &c, VERIFIER).await;
        assert_eq!(status, StatusCode::OK);
        let (_, claims) = claims(body["id_token"].as_str().unwrap());
        assert_eq!(claims["tid"], CONSUMER_TENANT_ID);
        assert_eq!(claims["preferred_username"], "sam@example.invalid");
        assert_eq!(account.kind, AccountKind::Personal);
    }

    /// The ID token is returned only when `openid` was asked for, unless the switch says
    /// always; the refresh token only with `offline_access`.
    #[tokio::test]
    async fn id_and_refresh_tokens_follow_the_scopes() {
        let (sim, _) = work_sim().await;
        let (_, location) = authorize(&sim, &[("scope", "Files.ReadWrite.All User.Read")]).await;
        let c = param(&location, "code").unwrap();
        let (status, body) = redeem(&sim, &c, VERIFIER).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.get("id_token").is_none(), "{body}");
        assert!(body.get("refresh_token").is_none(), "{body}");
        assert_eq!(body["scope"], "Files.ReadWrite.All User.Read");

        sim.set_identity_options(IdentityOptions {
            id_token: IdTokenIssue::Always,
            ..IdentityOptions::default()
        });
        let (_, location) = authorize(&sim, &[("scope", "User.Read")]).await;
        let (_, body) = redeem(&sim, &param(&location, "code").unwrap(), VERIFIER).await;
        assert!(body["id_token"].is_string());
    }

    /// AC2, ADR-0010 R8 (the authority's side): a refresh returns a new refresh token; by
    /// default the old one is refused afterwards, and with the switch it keeps working.
    #[tokio::test]
    async fn ac2_refresh_rolls_the_refresh_token() {
        let (sim, _) = work_sim().await;
        let c = code(&sim).await;
        let (_, first) = redeem(&sim, &c, VERIFIER).await;
        let old = first["refresh_token"].as_str().unwrap().to_owned();

        let (status, second) = refresh(&sim, &old).await;
        assert_eq!(status, StatusCode::OK);
        let new = second["refresh_token"].as_str().unwrap().to_owned();
        assert_ne!(new, old);
        assert_ne!(second["access_token"], first["access_token"]);

        let (status, body) = refresh(&sim, &old).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::BAD_REQUEST, Some("invalid_grant"))
        );
        let (status, _) = refresh(&sim, &new).await;
        assert_eq!(status, StatusCode::OK);

        sim.set_identity_options(IdentityOptions {
            old_refresh_token: OldRefreshToken::Accepted,
            ..IdentityOptions::default()
        });
        let c = code(&sim).await;
        let (_, first) = redeem(&sim, &c, VERIFIER).await;
        let old = first["refresh_token"].as_str().unwrap().to_owned();
        let (status, _) = refresh(&sim, &old).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = refresh(&sim, &old).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "the used token keeps working under Accepted"
        );
    }

    /// AC2: a revoked refresh token answers `invalid_grant`, for one token or every token
    /// of the account; an expired one too.
    #[tokio::test]
    async fn ac2_revoked_refresh_token_is_invalid_grant() {
        let (sim, account) = work_sim().await;
        let (_, a) = redeem(&sim, &code(&sim).await, VERIFIER).await;
        let (_, b) = redeem(&sim, &code(&sim).await, VERIFIER).await;
        let a = a["refresh_token"].as_str().unwrap().to_owned();
        let b = b["refresh_token"].as_str().unwrap().to_owned();

        assert!(sim.revoke_refresh_token(&a));
        let (status, body) = refresh(&sim, &a).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::BAD_REQUEST, Some("invalid_grant"))
        );

        sim.revoke_refresh_tokens(&account);
        let (status, body) = refresh(&sim, &b).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::BAD_REQUEST, Some("invalid_grant"))
        );

        let (_, c) = redeem(&sim, &code(&sim).await, VERIFIER).await;
        let c = c["refresh_token"].as_str().unwrap().to_owned();
        sim.advance_clock(u32::try_from(REFRESH_TOKEN_LIFETIME_SECS + 1).unwrap());
        let (status, body) = refresh(&sim, &c).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::BAD_REQUEST, Some("invalid_grant"))
        );
        assert!(!sim.revoke_refresh_token("sim-rt-not-issued.x"));
    }

    /// An access token the authority issues is accepted by the Graph origin and answers
    /// `/me` for its account; revoking it gives 401. The fixed token keeps working.
    #[tokio::test]
    async fn issued_access_tokens_work_on_graph_beside_the_fixed_token() {
        let (sim, account) = work_sim().await;
        let (_, body) = redeem(&sim, &code(&sim).await, VERIFIER).await;
        let access = body["access_token"].as_str().unwrap().to_owned();

        let (status, me) = graph_get(&sim, "/me", &access).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(me["userPrincipalName"], "priya@contoso.example.invalid");
        assert_eq!(me["id"], account.object_id.as_str());

        let (status, _) = graph_get(&sim, &format!("/drives/{}", account.drive), sim.token()).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "the fixed token still reaches every drive"
        );
        let (status, _) = graph_get(&sim, "/me", sim.token()).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "the fixed token is not an account's"
        );
        sim.bind_token(sim.token(), &account).unwrap();
        let (status, _) = graph_get(&sim, "/me", sim.token()).await;
        assert_eq!(status, StatusCode::OK);

        sim.revoke_token(&access);
        let (status, _) = graph_get(&sim, "/me", &access).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    /// AC6: a personal account's `/me/drive` is a personal drive and a work account's a
    /// business drive, each the account's own. (The simulator has no mixed-case drive ID
    /// switch, so IDs are as `add_drive` makes them.)
    #[tokio::test]
    async fn ac6_own_drive_matches_the_account_kind() {
        let sim = GraphSim::start().await.unwrap();
        let tenant = sim.add_tenant("Contoso", "contoso.example.invalid");
        let work = sim
            .add_work_account(&tenant, "priya@contoso.example.invalid", "Priya Example")
            .unwrap();
        let personal = sim
            .add_personal_account("sam@example.invalid", "Sam Example")
            .unwrap();
        let (work_token, personal_token) = ("work-token", "personal-token");
        sim.add_token(work_token);
        sim.add_token(personal_token);
        sim.bind_token(work_token, &work).unwrap();
        sim.bind_token(personal_token, &personal).unwrap();

        let (status, drive) = graph_get(&sim, "/me/drive", personal_token).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(drive["driveType"], DriveType::Personal.as_wire());
        assert_eq!(drive["id"], personal.drive.as_str());

        let (status, drive) = graph_get(&sim, "/me/drive", work_token).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(drive["driveType"], DriveType::Business.as_wire());
        assert_eq!(drive["id"], work.drive.as_str());

        let (_, me) = graph_get(&sim, "/me", personal_token).await;
        assert_eq!(me["id"], personal.drive.as_str());
        assert_eq!(me["displayName"], "Sam Example");
    }

    /// `/organization` names a work account's tenant; for a personal account it answers
    /// 403 by default and an empty list under the switch.
    #[tokio::test]
    async fn organization_follows_account_kind_and_switch() {
        let sim = GraphSim::start().await.unwrap();
        let tenant = sim.add_tenant("Contoso", "contoso.example.invalid");
        let work = sim
            .add_work_account(&tenant, "priya@contoso.example.invalid", "Priya Example")
            .unwrap();
        let personal = sim
            .add_personal_account("sam@example.invalid", "Sam Example")
            .unwrap();
        sim.add_token("w");
        sim.add_token("p");
        sim.bind_token("w", &work).unwrap();
        sim.bind_token("p", &personal).unwrap();

        let (status, org) = graph_get(&sim, "/organization", "w").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(org["value"][0]["id"], tenant.as_str());
        assert_eq!(org["value"][0]["displayName"], "Contoso");
        assert_eq!(
            org["value"][0]["verifiedDomains"][0]["name"],
            "contoso.example.invalid"
        );

        let (status, _) = graph_get(&sim, "/organization", "p").await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        sim.set_identity_options(IdentityOptions {
            personal_organization: PersonalOrganization::Empty,
            ..IdentityOptions::default()
        });
        let (status, org) = graph_get(&sim, "/organization", "p").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(org["value"].as_array().unwrap().len(), 0);
    }

    /// The tenant registry groups accounts and drives.
    #[tokio::test]
    async fn tenants_group_accounts_and_drives() {
        let sim = GraphSim::start().await.unwrap();
        let contoso = sim.add_tenant("Contoso", "contoso.example.invalid");
        let fabrikam = sim.add_tenant("Fabrikam", "fabrikam.example.invalid");
        let a = sim
            .add_work_account(&contoso, "a@contoso.example.invalid", "A")
            .unwrap();
        let b = sim
            .add_work_account(&contoso, "b@contoso.example.invalid", "B")
            .unwrap();
        let c = sim
            .add_work_account(&fabrikam, "c@fabrikam.example.invalid", "C")
            .unwrap();
        let site = sim.add_site(&contoso, "Finance").unwrap();
        let library = sim.add_library(&site).unwrap();
        let personal = sim.add_personal_account("p@example.invalid", "P").unwrap();

        assert_eq!(sim.tenant_accounts(&contoso), vec![a.clone(), b.clone()]);
        assert_eq!(sim.tenant_accounts(&fabrikam), vec![c.clone()]);
        let mut expected = vec![a.drive, b.drive, library.clone()];
        expected.sort();
        assert_eq!(sim.tenant_drives(&contoso), expected);
        assert_eq!(sim.tenant_of_drive(&library), Some(contoso.clone()));
        assert_eq!(sim.tenant_of_drive(&c.drive), Some(fabrikam));
        assert_eq!(sim.tenant_of_drive(&personal.drive), None);
        assert_ne!(a.object_id, b.object_id);
    }

    /// The authorise step can be made to redirect with an error, and refuses to redirect
    /// without a client ID or redirect URI or a chosen account.
    #[tokio::test]
    async fn authorise_errors() {
        let sim = GraphSim::start().await.unwrap();
        let (status, _) = authorize(&sim, &[]).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "no account is signing in");

        let account = sim
            .add_personal_account("sam@example.invalid", "Sam")
            .unwrap();
        sim.sign_in_as(&account).unwrap();
        sim.deny_next_sign_in("access_denied", "The user cancelled.");
        let (status, location) = authorize(&sim, &[]).await;
        assert_eq!(status, StatusCode::FOUND);
        assert_eq!(param(&location, "error").as_deref(), Some("access_denied"));
        assert_eq!(param(&location, "state").as_deref(), Some("state-1"));
        assert!(param(&location, "code").is_none());
        // Only the next one.
        let (status, location) = authorize(&sim, &[]).await;
        assert_eq!(status, StatusCode::FOUND);
        assert!(param(&location, "code").is_some());

        let response = client()
            .get(format!(
                "{}oauth2/v2.0/authorize?response_type=code",
                sim.authority_url()
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    fn log_text(sim: &GraphSim) -> String {
        format!("{:?}", sim.requests())
    }

    /// AC7, ADR-0010 R6 (the simulator's log): no access token, refresh token, code or
    /// ID token the authority issues is in the request log in full, on any origin; the
    /// log keeps the issued values' non-secret labels. The fixed token is logged as before.
    #[tokio::test]
    async fn ac7_issued_secrets_never_reach_the_log() {
        let (sim, _) = work_sim().await;
        let c = code(&sim).await;
        let (_, body) = redeem(&sim, &c, VERIFIER).await;
        let access = body["access_token"].as_str().unwrap().to_owned();
        let refresh_token = body["refresh_token"].as_str().unwrap().to_owned();
        let id_token = body["id_token"].as_str().unwrap().to_owned();
        let (_, rolled) = refresh(&sim, &refresh_token).await;
        let rolled = rolled["refresh_token"].as_str().unwrap().to_owned();

        // On the Graph origin: as bearer token, in a query string and in a path.
        graph_get(&sim, "/me", &access).await;
        graph_get(&sim, &format!("/me?leak={access}&id={id_token}"), &access).await;
        graph_get(&sim, &format!("/nowhere/{refresh_token}"), &access).await;
        let graph: Vec<_> = sim
            .requests()
            .into_iter()
            .filter(|r| r.origin == Origin::Graph)
            .collect();
        let label = &access[..=access.find('.').unwrap()];
        assert_eq!(
            graph[0].header("authorization"),
            Some(format!("Bearer {label}<redacted>").as_str())
        );
        graph_get(&sim, "/me", sim.token()).await;
        let last = sim.requests().pop().unwrap();
        assert_eq!(
            last.header("authorization"),
            Some(format!("Bearer {}", sim.token()).as_str()),
            "the fixed token is logged in full, as before"
        );

        // On the authority: secrets planted in the authorise query, in the token form
        // body (never logged) and in a stray Authorization header.
        let url = format!(
            "{}oauth2/v2.0/authorize?client_id={CLIENT_ID}&redirect_uri={}&response_type=code\
             &code=PLANTED-CODE&code_verifier=PLANTED-VERIFIER&refresh_token=PLANTED-RT\
             &access_token=PLANTED-AT&id_token=PLANTED-ID&x={rolled}",
            sim.authority_url(),
            encode(REDIRECT)
        );
        let response = client()
            .get(url)
            .header(AUTHORIZATION, "Bearer PLANTED-BEARER")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);
        let authority: Vec<_> = sim
            .requests()
            .into_iter()
            .filter(|r| r.origin == Origin::Authority)
            .collect();
        assert!(authority.len() >= 4, "authorise, token, refresh, planted");
        let planted = authority.last().unwrap();
        assert_eq!(planted.header("authorization"), Some("Bearer <redacted>"));
        assert!(
            planted
                .query
                .as_deref()
                .unwrap()
                .contains("code=<redacted>")
        );

        let text = log_text(&sim);
        for secret in [
            c.as_str(),
            VERIFIER,
            access.as_str(),
            refresh_token.as_str(),
            rolled.as_str(),
            id_token.as_str(),
            "PLANTED-CODE",
            "PLANTED-VERIFIER",
            "PLANTED-RT",
            "PLANTED-AT",
            "PLANTED-ID",
            "PLANTED-BEARER",
        ] {
            assert!(!text.contains(secret), "{secret} is in the log: {text}");
        }
        assert!(text.contains(label), "the log keeps the token's label");
    }

    /// AC8: `docs/identity.md` gives a source column for each behaviour, marks the blocked
    /// and read-only signals as unverified pending gate G2, names every switch, says
    /// where the policy layer sits, and is in the README's index.
    #[test]
    fn ac8_identity_doc_sources_and_provisional_signals() {
        let doc = include_str!("../docs/identity.md");
        let section = doc
            .split("### Blocked and read-only libraries: provisional until gate G2")
            .nth(1)
            .expect("the doc has the provisional section");
        let section = section.split("\n## ").next().unwrap();
        assert!(section.contains("LibraryAccess::Blocked"));
        assert!(section.contains("LibraryAccess::ReadOnly"));
        assert!(
            section.matches("**Unverified**").count() + section.matches("**unverified**").count()
                >= 2
        );
        assert!(section.contains("G2"));
        for switch in [
            "old_refresh_token",
            "id_token = Always",
            "personal_organization",
            "share_without_prefer",
        ] {
            assert!(doc.contains(switch), "{switch}");
        }
        assert!(doc.contains("The download origin is **not** covered"));
        assert!(doc.contains("9188040d-6c67-4c5b-b112-36a304b66dad"));
        // Every row of every behaviour table has a non-empty source cell.
        for line in doc
            .lines()
            .filter(|l| l.starts_with("| ") && !l.starts_with("| Behaviour"))
        {
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            assert!(
                cells.len() >= 4 && !cells[2].is_empty(),
                "no source: {line}"
            );
        }
        let readme = include_str!("../README.md");
        assert!(readme.contains("[`docs/identity.md`](docs/identity.md) | `src/identity.rs`"));
    }
}

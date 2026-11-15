//! Self-tests for discovery and library policy (WP-023), using a plain HTTP client. Each
//! acceptance criterion has a test named `acN_...`.

// `cfg(test)` tells clippy the helpers are test code, so they may unwrap.
#[cfg(test)]
mod discovery_tests {
    use mydrive_core::ids::DriveId;
    use mydrive_graph_sim::{
        Account, DEFAULT_POLICY_CODE, GraphSim, IdentityOptions, LibraryAccess, Method, Origin,
        ShareWithoutPrefer, StatusCode, encode_sharing_url,
    };
    use reqwest::redirect::Policy;
    use serde_json::Value;

    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .redirect(Policy::none())
            .build()
            .unwrap()
    }

    async fn send(
        sim: &GraphSim,
        method: Method,
        path: &str,
        token: &str,
        prefer: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut request = client()
            .request(method, format!("{}{path}", sim.base_url()))
            .bearer_auth(token);
        if let Some(prefer) = prefer {
            request = request.header("Prefer", prefer);
        }
        let response = request.send().await.unwrap();
        let status = response.status();
        let bytes = response.bytes().await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn get(sim: &GraphSim, path: &str, token: &str) -> (StatusCode, Value) {
        send(sim, Method::GET, path, token, None).await
    }

    struct Setup {
        sim: GraphSim,
        priya: Account,
        token: &'static str,
        finance: DriveId,
        atlas: DriveId,
    }

    /// A tenant with a work account (bound to `token`) and two libraries on two sites.
    async fn setup() -> Setup {
        let sim = GraphSim::start().await.unwrap();
        let tenant = sim.add_tenant("Contoso", "contoso.example.invalid");
        let priya = sim
            .add_work_account(&tenant, "priya@contoso.example.invalid", "Priya Example")
            .unwrap();
        let finance = sim
            .add_library(&sim.add_site(&tenant, "Finance").unwrap())
            .unwrap();
        let atlas = sim
            .add_library(&sim.add_site(&tenant, "Project Atlas").unwrap())
            .unwrap();
        let token = "priya-token";
        sim.add_token(token);
        sim.bind_token(token, &priya).unwrap();
        Setup {
            sim,
            priya,
            token,
            finance,
            atlas,
        }
    }

    fn share_path(link: &str) -> String {
        format!("/shares/{}/driveItem", encode_sharing_url(link))
    }

    /// AC3, ADR-0015 R2 (the simulator's side): resolving with
    /// `redeemSharingLinkIfNecessary` returns the item but leaves the drive unreachable;
    /// `redeemSharingLink` makes it reachable. The log records the `Prefer` value sent.
    #[tokio::test]
    async fn ac3_only_redeem_grants_durable_access() {
        let s = setup().await;
        let root = s.sim.root_id(&s.finance).unwrap();
        let link = s.sim.add_sharing_link(&s.finance, &root).unwrap();
        assert!(link.contains(".example.invalid/"), "{link}");
        let drive_path = format!("/drives/{}", s.finance);

        let (status, _) = get(&s.sim, &drive_path, s.token).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "not reachable before any redeem"
        );

        let (status, item) = send(
            &s.sim,
            Method::GET,
            &share_path(&link),
            s.token,
            Some("redeemSharingLinkIfNecessary"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(item["id"], root.as_str());
        assert_eq!(item["parentReference"]["driveId"], s.finance.as_str());
        assert_eq!(item["parentReference"]["driveType"], "documentLibrary");
        let site = s.sim.site_of_library(&s.finance).unwrap();
        assert_eq!(item["parentReference"]["siteId"], site.as_str());

        let (status, _) = get(&s.sim, &drive_path, s.token).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "a peek grants nothing durable"
        );
        assert!(!s.sim.can_reach(&s.priya, &s.finance));

        let (status, _) = send(
            &s.sim,
            Method::GET,
            &share_path(&link),
            s.token,
            Some("redeemSharingLink"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, drive) = get(&s.sim, &drive_path, s.token).await;
        assert_eq!(status, StatusCode::OK, "redeemed: reachable");
        assert_eq!(drive["driveType"], "documentLibrary");
        assert!(s.sim.can_reach(&s.priya, &s.finance));

        let prefers: Vec<_> = s
            .sim
            .requests()
            .into_iter()
            .filter(|r| r.path.contains("/shares/"))
            .map(|r| r.header("prefer").map(str::to_owned))
            .collect();
        assert_eq!(
            prefers,
            vec![
                Some("redeemSharingLinkIfNecessary".to_owned()),
                Some("redeemSharingLink".to_owned())
            ]
        );
    }

    /// Without `Prefer`, an account that cannot reach the drive is refused by default, and
    /// gets the item without durable access under the switch. Another tenant's account is
    /// refused; an unknown link is 404.
    #[tokio::test]
    async fn share_without_prefer_and_other_refusals() {
        let s = setup().await;
        let root = s.sim.root_id(&s.finance).unwrap();
        let link = s.sim.add_sharing_link(&s.finance, &root).unwrap();

        let (status, _) = get(&s.sim, &share_path(&link), s.token).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        s.sim.set_identity_options(IdentityOptions {
            share_without_prefer: ShareWithoutPrefer::Peek,
            ..IdentityOptions::default()
        });
        let (status, _) = get(&s.sim, &share_path(&link), s.token).await;
        assert_eq!(status, StatusCode::OK);
        assert!(!s.sim.can_reach(&s.priya, &s.finance));

        let other = s.sim.add_tenant("Fabrikam", "fabrikam.example.invalid");
        let outsider = s
            .sim
            .add_work_account(&other, "o@fabrikam.example.invalid", "Outsider")
            .unwrap();
        s.sim.add_token("outsider");
        s.sim.bind_token("outsider", &outsider).unwrap();
        let (status, _) = send(
            &s.sim,
            Method::GET,
            &share_path(&link),
            "outsider",
            Some("redeemSharingLink"),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(!s.sim.can_reach(&outsider, &s.finance));

        let (status, _) = get(
            &s.sim,
            &share_path("https://contoso.sharepoint.example.invalid/:f:/s/X/Nope"),
            s.token,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = get(&s.sim, "/shares/not-encoded/driveItem", s.token).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    /// The site and library names that name a mount: `/sites/{id}`, `/drives/{id}/list`,
    /// and the drive resource.
    #[tokio::test]
    async fn site_and_library_names() {
        let s = setup().await;
        s.sim.grant_access(&s.priya, &s.atlas).unwrap();
        let site = s.sim.site_of_library(&s.atlas).unwrap();
        assert!(
            site.as_str()
                .starts_with("contoso.sharepoint.example.invalid,")
        );

        let (status, body) = get(&s.sim, &format!("/sites/{site}"), s.token).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["displayName"], "Project Atlas");
        assert_eq!(body["id"], site.as_str());
        assert_eq!(
            body["webUrl"],
            "https://contoso.sharepoint.example.invalid/sites/ProjectAtlas"
        );

        let (status, list) = get(&s.sim, &format!("/drives/{}/list", s.atlas), s.token).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(list["displayName"], "Documents");
        assert_eq!(list["list"]["template"], "documentLibrary");
        assert_eq!(list["parentReference"]["siteId"], site.as_str());

        let (_, drive) = get(&s.sim, &format!("/drives/{}", s.atlas), s.token).await;
        assert_eq!(drive["name"], "Documents");

        // A drive that is not a library has no list.
        let (status, _) = get(&s.sim, &format!("/drives/{}/list", s.priya.drive), s.token).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    /// The routes a client uses on a drive, including one that does not exist.
    fn drive_paths(sim: &GraphSim, drive: &DriveId) -> Vec<String> {
        let root = sim.root_id(drive).unwrap();
        vec![
            format!("/drives/{drive}"),
            format!("/drives/{drive}/root"),
            format!("/drives/{drive}/root/children"),
            format!("/drives/{drive}/root/delta"),
            format!("/drives/{drive}/items/{root}"),
            format!("/drives/{drive}/items/{root}/children"),
            format!("/drives/{drive}/list"),
            format!("/drives/{drive}/no-such-route"),
            format!("/drives/{}", drive.as_str().replace('!', "%21")),
            format!("/drives/{}/root", drive.as_str().to_ascii_uppercase()),
        ]
    }

    /// AC4, ADR-0015 R4 (the simulator's side): a blocked library answers 403 with the
    /// configured code on every route, while another library of the same tenant works.
    /// Provisional until gate G2.
    #[tokio::test]
    async fn ac4_blocked_library_refuses_every_route_others_work() {
        let s = setup().await;
        s.sim.grant_access(&s.priya, &s.finance).unwrap();
        s.sim.grant_access(&s.priya, &s.atlas).unwrap();
        let code = "simulatedPolicyBlock";
        s.sim
            .set_library_access(
                &s.atlas,
                LibraryAccess::Blocked {
                    code: code.to_owned(),
                },
            )
            .unwrap();

        for token in [s.token, s.sim.token()] {
            for path in drive_paths(&s.sim, &s.atlas) {
                for method in [Method::GET, Method::PUT, Method::DELETE] {
                    let (status, body) = send(&s.sim, method.clone(), &path, token, None).await;
                    assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}");
                    assert_eq!(body["error"]["code"], code, "{method} {path}");
                }
            }
            for path in drive_paths(&s.sim, &s.finance)
                .into_iter()
                .filter(|p| !p.contains("no-such-route"))
            {
                let (status, _) = get(&s.sim, &path, token).await;
                assert_eq!(status, StatusCode::OK, "GET {path} on the other library");
            }
        }
        // A sharing link into the blocked library is refused too.
        let root = s.sim.root_id(&s.atlas).unwrap();
        let link = s.sim.add_sharing_link(&s.atlas, &root).unwrap();
        let (status, body) = send(
            &s.sim,
            Method::GET,
            &share_path(&link),
            s.token,
            Some("redeemSharingLinkIfNecessary"),
        )
        .await;
        assert_eq!(
            (status, body["error"]["code"].as_str()),
            (StatusCode::FORBIDDEN, Some(code))
        );
        assert!(
            s.sim.requests().iter().all(|r| r.origin == Origin::Graph),
            "nothing reached another origin"
        );

        // Lifting the block restores access.
        s.sim
            .set_library_access(&s.atlas, LibraryAccess::Normal)
            .unwrap();
        let (status, _) = get(&s.sim, &format!("/drives/{}/root", s.atlas), s.token).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(s.sim.library_access(&s.atlas), LibraryAccess::Normal);
    }

    /// The read-only layer (ADR-0015 R5, the simulator's side; provisional until G2): any
    /// method other than GET and HEAD is refused with 403 and the configured code, on any
    /// route below the drive, including write routes that do not exist yet; reads work.
    /// On a normal library the same requests are not refused by the layer.
    #[tokio::test]
    async fn read_only_library_refuses_every_writing_method() {
        let s = setup().await;
        s.sim.grant_access(&s.priya, &s.finance).unwrap();
        s.sim.grant_access(&s.priya, &s.atlas).unwrap();
        s.sim
            .set_library_access(&s.finance, LibraryAccess::read_only())
            .unwrap();
        let root = s.sim.root_id(&s.finance).unwrap();
        let other_root = s.sim.root_id(&s.atlas).unwrap();
        let writes = [
            Method::PUT,
            Method::POST,
            Method::PATCH,
            Method::DELETE,
            Method::from_bytes(b"MOVE").unwrap(),
        ];
        let paths = |drive: &DriveId, root: &str| {
            vec![
                format!("/drives/{drive}/items/{root}:/new.txt:/content"),
                format!("/drives/{drive}/items/{root}/children"),
                format!("/drives/{drive}/items/{root}"),
                format!("/drives/{drive}/items/{root}/createUploadSession"),
            ]
        };
        for path in paths(&s.finance, root.as_str()) {
            for method in &writes {
                let (status, body) = send(&s.sim, method.clone(), &path, s.token, None).await;
                assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}");
                assert_eq!(
                    body["error"]["code"], DEFAULT_POLICY_CODE,
                    "{method} {path}"
                );
            }
        }
        for path in paths(&s.atlas, other_root.as_str()) {
            for method in &writes {
                let (status, _) = send(&s.sim, method.clone(), &path, s.token, None).await;
                assert_ne!(
                    status,
                    StatusCode::FORBIDDEN,
                    "{method} {path} on a normal library"
                );
            }
        }
        for method in [Method::GET, Method::HEAD] {
            let (status, _) = send(
                &s.sim,
                method.clone(),
                &format!("/drives/{}/root/children", s.finance),
                s.token,
                None,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{method} on a read-only library");
        }
    }

    /// The layer runs inside the bearer check: an unauthenticated request to a blocked
    /// library is 401, not 403. A drive that does not exist stays 404.
    #[tokio::test]
    async fn policy_runs_after_the_token_check() {
        let s = setup().await;
        s.sim
            .set_library_access(&s.atlas, LibraryAccess::blocked())
            .unwrap();
        let (status, _) = get(&s.sim, &format!("/drives/{}", s.atlas), "unknown-token").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = get(&s.sim, "/drives/b!no-such-drive", s.token).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}

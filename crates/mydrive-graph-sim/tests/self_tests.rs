//! Self-tests for the Graph simulator (WP-007), using a plain HTTP client.
//!
//! Each acceptance criterion has a test named `acN_...`.

// The helpers below unwrap like the tests do; `cfg(test)` tells clippy they are test code
// (`allow-unwrap-in-tests` in `clippy.toml`).
#[cfg(test)]
mod self_tests {
    use std::collections::HashSet;
    use std::time::Duration;

    use mydrive_graph_sim::{
        DeltaRepeats, DownloadMode, DriveOptions, DriveType, Fault, GraphSim, Method, Origin,
        RangeMode, StatusCode,
    };
    use reqwest::redirect::Policy;
    use serde_json::Value;

    /// A client that does not follow redirects, so tests see the 302 itself.
    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .redirect(Policy::none())
            .build()
            .unwrap()
    }

    async fn get_json(sim: &GraphSim, url: &str) -> Value {
        let response = client()
            .get(url)
            .bearer_auth(sim.token())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "GET {url}");
        serde_json::from_slice(&response.bytes().await.unwrap()).unwrap()
    }

    fn delta_url(sim: &GraphSim, drive: &impl std::fmt::Display) -> String {
        format!("{}/drives/{drive}/root/delta", sim.base_url())
    }

    /// Follows nextLinks from `url` to the deltaLink. Returns every page and the deltaLink.
    async fn full_delta(sim: &GraphSim, url: &str) -> (Vec<Value>, String) {
        let mut pages = Vec::new();
        let mut next = url.to_owned();
        loop {
            let page = get_json(sim, &next).await;
            let next_link = page["@odata.nextLink"].as_str().map(str::to_owned);
            let delta_link = page["@odata.deltaLink"].as_str().map(str::to_owned);
            pages.push(page);
            match (next_link, delta_link) {
                (Some(link), None) => next = link,
                (None, Some(link)) => return (pages, link),
                other => {
                    panic!("a page must carry exactly one of nextLink or deltaLink: {other:?}")
                }
            }
        }
    }

    fn items(pages: &[Value]) -> Vec<Value> {
        pages
            .iter()
            .flat_map(|page| page["value"].as_array().unwrap().clone())
            .collect()
    }

    fn ids(items: &[Value]) -> Vec<String> {
        items
            .iter()
            .map(|item| item["id"].as_str().unwrap().to_owned())
            .collect()
    }

    /// Asserts that no `parentReference` anywhere in `value` has a `path`.
    fn assert_no_parent_path(value: &Value) {
        match value {
            Value::Object(map) => {
                if let Some(reference) = map.get("parentReference") {
                    assert!(
                        reference.get("path").is_none(),
                        "parentReference has a path: {reference}"
                    );
                }
                map.values().for_each(assert_no_parent_path);
            }
            Value::Array(values) => values.iter().for_each(assert_no_parent_path),
            _ => {}
        }
    }

    /// AC1: a test can build a tree, page through a full delta to a deltaLink, and calling the
    /// deltaLink returns only what changed since.
    #[tokio::test]
    async fn ac1_delta_pages_to_delta_link_then_returns_only_changes() {
        let sim = GraphSim::start().await.unwrap();
        let drive = sim
            .add_drive_with(
                DriveType::Personal,
                DriveOptions {
                    delta_page_size: 2,
                    ..DriveOptions::default()
                },
            )
            .unwrap();
        let root = sim.root_id(&drive).unwrap();
        let docs = sim.create_folder(&drive, &root, "Documents").unwrap();
        let a = sim
            .create_file(&drive, &docs, "a.txt", b"aaa".to_vec())
            .unwrap();
        let b = sim
            .create_file(&drive, &docs, "b.txt", b"bbb".to_vec())
            .unwrap();
        let c = sim
            .create_file(&drive, &root, "c.txt", b"ccc".to_vec())
            .unwrap();

        let (pages, delta_link) = full_delta(&sim, &delta_url(&sim, &drive)).await;
        assert_eq!(pages.len(), 3, "5 items at 2 per page");
        let first = items(&pages);
        let seen: HashSet<String> = ids(&first).into_iter().collect();
        let expected: HashSet<String> = [&root, &docs, &a, &b, &c]
            .iter()
            .map(|id| id.to_string())
            .collect();
        assert_eq!(seen, expected);
        assert!(first.iter().any(|item| item.get("root").is_some()));

        sim.advance_clock(60);
        sim.modify_file(&drive, &a, b"changed".to_vec()).unwrap();
        let d = sim
            .create_file(&drive, &root, "d.txt", b"new".to_vec())
            .unwrap();

        let (pages, second_link) = full_delta(&sim, &delta_link).await;
        let changed = items(&pages);
        assert_eq!(ids(&changed), vec![a.to_string(), d.to_string()]);
        assert_eq!(changed[0]["size"], 7);
        assert_eq!(changed[0]["lastModifiedDateTime"], "2026-01-01T00:01:00Z");

        let (pages, _) = full_delta(&sim, &second_link).await;
        assert!(
            items(&pages).is_empty(),
            "nothing changed since the last deltaLink"
        );
    }

    /// An item changed twice since the token appears twice in one feed (the delta docs' "same
    /// item may appear more than once"); with `LastChangeOnly` it appears once. Supports
    /// clients implementing ADR-0011 R2.
    #[tokio::test]
    async fn item_changed_twice_appears_twice_in_one_feed() {
        let sim = GraphSim::start().await.unwrap();
        let drive = sim.add_drive(DriveType::Business).unwrap();
        let root = sim.root_id(&drive).unwrap();
        let file = sim
            .create_file(&drive, &root, "f.txt", b"1".to_vec())
            .unwrap();
        let (_, link) = full_delta(&sim, &delta_url(&sim, &drive)).await;

        sim.modify_file(&drive, &file, b"22".to_vec()).unwrap();
        sim.rename(&drive, &file, "g.txt").unwrap();
        let (pages, _) = full_delta(&sim, &link).await;
        let changed = items(&pages);
        assert_eq!(ids(&changed), vec![file.to_string(), file.to_string()]);
        assert_eq!(changed[1]["name"], "g.txt");

        let mut options = sim.options(&drive).unwrap();
        options.delta_repeats = DeltaRepeats::LastChangeOnly;
        sim.set_options(&drive, options).unwrap();
        let (pages, _) = full_delta(&sim, &link).await;
        assert_eq!(ids(&items(&pages)), vec![file.to_string()]);
    }

    /// AC2: delta for a business drive contains no cTag, and no response has a path in
    /// `parentReference`.
    #[tokio::test]
    async fn ac2_business_delta_has_no_ctag_and_no_response_has_parent_path() {
        let sim = GraphSim::start().await.unwrap();
        let business = sim.add_drive(DriveType::Business).unwrap();
        let personal = sim.add_drive(DriveType::Personal).unwrap();
        for drive in [&business, &personal] {
            let root = sim.root_id(drive).unwrap();
            let folder = sim.create_folder(drive, &root, "Folder").unwrap();
            sim.create_file(drive, &folder, "x.bin", vec![1u8; 10])
                .unwrap();
            let gone = sim
                .create_file(drive, &folder, "gone.bin", vec![2u8; 3])
                .unwrap();
            sim.delete(drive, &gone).unwrap();
        }

        let (pages, _) = full_delta(&sim, &delta_url(&sim, &business)).await;
        for page in &pages {
            assert_no_parent_path(page);
        }
        let business_items = items(&pages);
        assert!(!business_items.is_empty());
        for item in &business_items {
            assert!(
                item.get("cTag").is_none(),
                "business delta item has cTag: {item}"
            );
        }
        // A business deleted item omits cTag and name (delta, "properties omitted"). It
        // appears once for its creation and once for its deletion, both in its latest state.
        let deleted: Vec<&Value> = business_items
            .iter()
            .filter(|i| i.get("deleted").is_some())
            .collect();
        assert_eq!(deleted.len(), 2);
        assert_eq!(deleted[0], deleted[1]);
        assert!(deleted[0].get("name").is_none());
        assert_eq!(deleted[0]["size"], 3);

        // Personal delta keeps cTag on live items; its deleted item omits cTag and size.
        let (pages, _) = full_delta(&sim, &delta_url(&sim, &personal)).await;
        let personal_items = items(&pages);
        for page in &pages {
            assert_no_parent_path(page);
        }
        for item in &personal_items {
            if item.get("deleted").is_some() {
                assert!(item.get("cTag").is_none() && item.get("size").is_none());
                assert_eq!(item["name"], "gone.bin");
            } else {
                assert!(
                    item.get("cTag").is_some(),
                    "personal delta item lacks cTag: {item}"
                );
            }
        }

        // Get item and list children carry no path either, by default.
        for drive in [&business, &personal] {
            let root = sim.root_id(drive).unwrap();
            let base = format!("{}/drives/{drive}", sim.base_url());
            assert_no_parent_path(&get_json(&sim, &format!("{base}/root")).await);
            let children = get_json(&sim, &format!("{base}/root/children")).await;
            assert_no_parent_path(&children);
            let folder = children["value"][0]["id"].as_str().unwrap().to_owned();
            assert_no_parent_path(&get_json(&sim, &format!("{base}/items/{folder}")).await);
            assert_no_parent_path(
                &get_json(&sim, &format!("{base}/items/{folder}/children")).await,
            );
            assert_no_parent_path(&get_json(&sim, &format!("{base}/items/{root}")).await);
        }
    }

    /// With `parent_path_outside_delta`, get-item reports the path as real Graph does, and
    /// delta still never does.
    #[tokio::test]
    async fn parent_path_switch_affects_get_item_but_never_delta() {
        let sim = GraphSim::start().await.unwrap();
        let drive = sim
            .add_drive_with(
                DriveType::Personal,
                DriveOptions {
                    parent_path_outside_delta: true,
                    ..DriveOptions::default()
                },
            )
            .unwrap();
        let root = sim.root_id(&drive).unwrap();
        let folder = sim.create_folder(&drive, &root, "My Docs").unwrap();
        let file = sim
            .create_file(&drive, &folder, "f", b"x".to_vec())
            .unwrap();
        let item = get_json(
            &sim,
            &format!("{}/drives/{drive}/items/{file}", sim.base_url()),
        )
        .await;
        assert_eq!(item["parentReference"]["path"], "/drive/root:/My%20Docs");
        assert_eq!(item["parentReference"]["id"], folder.as_str());
        let (pages, _) = full_delta(&sim, &delta_url(&sim, &drive)).await;
        for page in &pages {
            assert_no_parent_path(page);
        }
    }

    /// AC3: renaming a folder appears in delta as that folder only, not its descendants.
    #[tokio::test]
    async fn ac3_folder_rename_appears_as_that_folder_only() {
        let sim = GraphSim::start().await.unwrap();
        let drive = sim.add_drive(DriveType::Personal).unwrap();
        let root = sim.root_id(&drive).unwrap();
        let top = sim.create_folder(&drive, &root, "Top").unwrap();
        let inner = sim.create_folder(&drive, &top, "Inner").unwrap();
        sim.create_file(&drive, &inner, "deep.txt", b"deep".to_vec())
            .unwrap();
        sim.create_file(&drive, &top, "shallow.txt", b"s".to_vec())
            .unwrap();
        let (_, link) = full_delta(&sim, &delta_url(&sim, &drive)).await;

        sim.rename(&drive, &top, "Renamed").unwrap();
        let (pages, _) = full_delta(&sim, &link).await;
        let changed = items(&pages);
        assert_eq!(ids(&changed), vec![top.to_string()]);
        assert_eq!(changed[0]["name"], "Renamed");
    }

    /// A deleted folder and everything below it appear with the `deleted` facet, the folder
    /// first, so a client must apply the whole feed before deciding the folder is empty.
    #[tokio::test]
    async fn deleted_items_carry_the_deleted_facet() {
        let sim = GraphSim::start().await.unwrap();
        let drive = sim.add_drive(DriveType::Personal).unwrap();
        let root = sim.root_id(&drive).unwrap();
        let folder = sim.create_folder(&drive, &root, "F").unwrap();
        let file = sim
            .create_file(&drive, &folder, "f", b"x".to_vec())
            .unwrap();
        let (_, link) = full_delta(&sim, &delta_url(&sim, &drive)).await;

        sim.delete(&drive, &folder).unwrap();
        let (pages, _) = full_delta(&sim, &link).await;
        let changed = items(&pages);
        assert_eq!(ids(&changed), vec![folder.to_string(), file.to_string()]);
        assert!(changed.iter().all(|item| item["deleted"].is_object()));

        let response = client()
            .get(format!("{}/drives/{drive}/items/{file}", sim.base_url()))
            .bearer_auth(sim.token())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        assert_eq!(body["error"]["code"], "itemNotFound");
    }

    /// Item JSON carries the quickXorHash from `mydrive-core`, whole-second timestamps,
    /// eTag and cTag, and the drive JSON its type.
    #[tokio::test]
    async fn item_and_drive_json_shapes() {
        let sim = GraphSim::start().await.unwrap();
        let drive = sim.add_drive(DriveType::Business).unwrap();
        let root = sim.root_id(&drive).unwrap();
        let content = b"The quick brown fox".to_vec();
        let file = sim
            .create_file(&drive, &root, "fox.txt", content.clone())
            .unwrap();

        let drive_json = get_json(&sim, &format!("{}/drives/{drive}", sim.base_url())).await;
        assert_eq!(drive_json["id"], drive.as_str());
        assert_eq!(drive_json["driveType"], "business");
        assert_eq!(drive_json["quota"]["used"], content.len());

        let item = get_json(
            &sim,
            &format!("{}/drives/{drive}/items/{file}", sim.base_url()),
        )
        .await;
        let mut hasher = mydrive_core::hash::QuickXorHasher::new();
        hasher.update(&content);
        assert_eq!(
            item["file"]["hashes"]["quickXorHash"],
            hasher.finalize().to_base64()
        );
        assert_eq!(item["size"], content.len());
        assert_eq!(item["createdDateTime"], "2026-01-01T00:00:00Z");
        assert!(item["eTag"].as_str().unwrap().starts_with("\"{"));
        // Business get-item keeps cTag on files; only delta omits it.
        assert!(item["cTag"].as_str().unwrap().starts_with("\"c:{"));
        assert_eq!(item["parentReference"]["driveId"], drive.as_str());
        assert_eq!(item["parentReference"]["driveType"], "business");
        assert!(item["@microsoft.graph.downloadUrl"].is_string());

        let root_json = get_json(&sim, &format!("{}/drives/{drive}/root", sim.base_url())).await;
        assert!(root_json["root"].is_object());
        assert_eq!(root_json["folder"]["childCount"], 1);
        assert!(
            root_json.get("cTag").is_none(),
            "business folders have no cTag"
        );
    }

    /// List children pages with `$top` and `@odata.nextLink`.
    #[tokio::test]
    async fn list_children_pages_with_next_link() {
        let sim = GraphSim::start().await.unwrap();
        let drive = sim.add_drive(DriveType::Personal).unwrap();
        let root = sim.root_id(&drive).unwrap();
        let mut created = Vec::new();
        for name in ["e", "d", "c", "b", "a"] {
            created.push(sim.create_file(&drive, &root, name, Vec::new()).unwrap());
        }
        let mut url = format!(
            "{}/drives/{drive}/items/{root}/children?$top=2",
            sim.base_url()
        );
        let mut names = Vec::new();
        let mut pages = 0;
        loop {
            let page = get_json(&sim, &url).await;
            pages += 1;
            for item in page["value"].as_array().unwrap() {
                names.push(item["name"].as_str().unwrap().to_owned());
            }
            match page["@odata.nextLink"].as_str() {
                Some(next) => url = next.to_owned(),
                None => break,
            }
        }
        assert_eq!(pages, 3);
        assert_eq!(names, vec!["a", "b", "c", "d", "e"]);
    }

    async fn ranged_get(url: &str, token: Option<&str>) -> reqwest::Response {
        let mut request = client().get(url).header("Range", "bytes=2-5");
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        request.send().await.unwrap()
    }

    /// AC4: with Range honoured a ranged request gets 206 and exactly those bytes; with Range
    /// ignored the same request gets 200 and the whole body. Checked in direct mode and on
    /// the pre-authenticated URL in redirect mode. Supports clients implementing ADR-0011 R11.
    #[tokio::test]
    async fn ac4_range_honoured_gives_206_and_ignored_gives_200() {
        let sim = GraphSim::start().await.unwrap();
        let drive = sim.add_drive(DriveType::Personal).unwrap();
        let root = sim.root_id(&drive).unwrap();
        let body = b"0123456789".to_vec();
        let file = sim
            .create_file(&drive, &root, "digits", body.clone())
            .unwrap();
        let content_url = format!("{}/drives/{drive}/items/{file}/content", sim.base_url());

        for download in [DownloadMode::Direct, DownloadMode::Redirect] {
            for range in [RangeMode::Honoured, RangeMode::Ignored] {
                let mut options = sim.options(&drive).unwrap();
                options.download = download;
                options.range = range;
                sim.set_options(&drive, options).unwrap();

                let url = match download {
                    DownloadMode::Direct => content_url.clone(),
                    DownloadMode::Redirect => {
                        let redirect = ranged_get(&content_url, Some(sim.token())).await;
                        assert_eq!(redirect.status(), StatusCode::FOUND);
                        redirect.headers()["location"].to_str().unwrap().to_owned()
                    }
                };
                let token = (download == DownloadMode::Direct).then(|| sim.token());
                let response = ranged_get(&url, token).await;
                match range {
                    RangeMode::Honoured => {
                        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
                        assert_eq!(response.headers()["content-range"], "bytes 2-5/10");
                        assert_eq!(&response.bytes().await.unwrap()[..], b"2345");
                    }
                    RangeMode::Ignored => {
                        assert_eq!(response.status(), StatusCode::OK);
                        assert!(response.headers().get("content-range").is_none());
                        assert_eq!(&response.bytes().await.unwrap()[..], &body[..]);
                    }
                }
            }
        }
    }

    /// A range outside the file gets 416 with `Content-Range: bytes */<len>`.
    #[tokio::test]
    async fn unsatisfiable_range_gets_416() {
        let sim = GraphSim::start().await.unwrap();
        let drive = sim
            .add_drive_with(
                DriveType::Personal,
                DriveOptions {
                    download: DownloadMode::Direct,
                    ..DriveOptions::default()
                },
            )
            .unwrap();
        let root = sim.root_id(&drive).unwrap();
        let file = sim
            .create_file(&drive, &root, "f", b"abc".to_vec())
            .unwrap();
        let response = client()
            .get(format!(
                "{}/drives/{drive}/items/{file}/content",
                sim.base_url()
            ))
            .bearer_auth(sim.token())
            .header("Range", "bytes=10-20")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(response.headers()["content-range"], "bytes */3");
    }

    /// AC5: in redirect mode the pre-authenticated URL works without a token; in direct mode
    /// a content request without a token gets 401.
    #[tokio::test]
    async fn ac5_redirect_url_needs_no_token_and_direct_needs_one() {
        let sim = GraphSim::start().await.unwrap();
        let drive = sim.add_drive(DriveType::Personal).unwrap();
        let root = sim.root_id(&drive).unwrap();
        let file = sim
            .create_file(&drive, &root, "f", b"payload".to_vec())
            .unwrap();
        let content_url = format!("{}/drives/{drive}/items/{file}/content", sim.base_url());

        // Redirect mode (the default).
        let redirect = client()
            .get(&content_url)
            .bearer_auth(sim.token())
            .send()
            .await
            .unwrap();
        assert_eq!(redirect.status(), StatusCode::FOUND);
        let location = redirect.headers()["location"].to_str().unwrap().to_owned();
        assert!(location.starts_with(sim.download_origin()));
        let download = client().get(&location).send().await.unwrap();
        assert_eq!(download.status(), StatusCode::OK);
        assert_eq!(&download.bytes().await.unwrap()[..], b"payload");
        // /content itself still needs the token in redirect mode.
        let unauthenticated = client().get(&content_url).send().await.unwrap();
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
        // A made-up pre-authenticated URL is refused.
        let forged = client()
            .get(format!(
                "{}/download?tempauth=forged",
                sim.download_origin()
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(forged.status(), StatusCode::UNAUTHORIZED);

        // Direct mode.
        let mut options = sim.options(&drive).unwrap();
        options.download = DownloadMode::Direct;
        sim.set_options(&drive, options).unwrap();
        let missing = client().get(&content_url).send().await.unwrap();
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
        let body: Value = serde_json::from_slice(&missing.bytes().await.unwrap()).unwrap();
        assert_eq!(body["error"]["code"], "InvalidAuthenticationToken");
        let with_token = client()
            .get(&content_url)
            .bearer_auth(sim.token())
            .send()
            .await
            .unwrap();
        assert_eq!(with_token.status(), StatusCode::OK);
        assert_eq!(&with_token.bytes().await.unwrap()[..], b"payload");
    }

    /// A missing, unknown or revoked bearer token gets 401 on every Graph endpoint.
    #[tokio::test]
    async fn unknown_or_revoked_token_gets_401() {
        let sim = GraphSim::start().await.unwrap();
        let drive = sim.add_drive(DriveType::Personal).unwrap();
        let url = format!("{}/drives/{drive}", sim.base_url());
        let status = |token: Option<&'static str>| {
            let url = url.clone();
            async move {
                let mut request = client().get(url);
                if let Some(token) = token {
                    request = request.bearer_auth(token);
                }
                request.send().await.unwrap().status()
            }
        };
        assert_eq!(status(None).await, StatusCode::UNAUTHORIZED);
        assert_eq!(status(Some("not-a-token")).await, StatusCode::UNAUTHORIZED);
        sim.add_token("extra");
        assert_eq!(status(Some("extra")).await, StatusCode::OK);
        sim.revoke_token("extra");
        assert_eq!(status(Some("extra")).await, StatusCode::UNAUTHORIZED);
    }

    /// AC6: a scripted fault applies to exactly the next matching request and then clears.
    #[tokio::test]
    async fn ac6_fault_applies_to_next_matching_request_only() {
        let sim = GraphSim::start().await.unwrap();
        let drive = sim.add_drive(DriveType::Personal).unwrap();
        let drive_url = format!("{}/drives/{drive}", sim.base_url());
        sim.push_fault(
            Fault::status(StatusCode::SERVICE_UNAVAILABLE)
                .retry_after(5)
                .on_method(Method::GET)
                .on_path("/root/delta"),
        );

        // A request that does not match leaves the fault queued.
        get_json(&sim, &drive_url).await;
        assert_eq!(sim.pending_faults(), 1);

        let faulted = client()
            .get(delta_url(&sim, &drive))
            .bearer_auth(sim.token())
            .send()
            .await
            .unwrap();
        assert_eq!(faulted.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(faulted.headers()["retry-after"], "5");
        let body: Value = serde_json::from_slice(&faulted.bytes().await.unwrap()).unwrap();
        assert_eq!(body["error"]["code"], "serviceNotAvailable");
        assert_eq!(sim.pending_faults(), 0);

        // The next matching request is served normally.
        let page = get_json(&sim, &delta_url(&sim, &drive)).await;
        assert!(page["@odata.deltaLink"].is_string());
    }

    /// A truncated body announces the full length and stops early; the client sees an error
    /// reading the body, and the next request is whole.
    #[tokio::test]
    async fn truncated_body_fault_cuts_the_download_short() {
        let sim = GraphSim::start().await.unwrap();
        let drive = sim
            .add_drive_with(
                DriveType::Personal,
                DriveOptions {
                    download: DownloadMode::Direct,
                    ..DriveOptions::default()
                },
            )
            .unwrap();
        let root = sim.root_id(&drive).unwrap();
        let file = sim
            .create_file(&drive, &root, "big", vec![7u8; 4096])
            .unwrap();
        let url = format!("{}/drives/{drive}/items/{file}/content", sim.base_url());
        sim.push_fault(Fault::truncate_body(100).on_path("/content"));

        let mut response = client()
            .get(&url)
            .bearer_auth(sim.token())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-length"], "4096");
        let mut received = 0;
        let ended = loop {
            match response.chunk().await {
                Ok(Some(chunk)) => received += chunk.len(),
                Ok(None) => break Ok(()),
                Err(err) => break Err(err),
            }
        };
        assert!(ended.is_err(), "a truncated body must not read as complete");
        assert_eq!(received, 100);
        assert_eq!(sim.pending_faults(), 0);

        let response = client()
            .get(&url)
            .bearer_auth(sim.token())
            .send()
            .await
            .unwrap();
        assert_eq!(response.bytes().await.unwrap().len(), 4096);
    }

    /// A delay fault holds the request for the given time, then serves it. Uses Tokio's paused
    /// clock, so the test does not wait in real time.
    #[tokio::test(start_paused = true)]
    async fn delay_fault_holds_the_request() {
        let sim = GraphSim::start().await.unwrap();
        let drive = sim.add_drive(DriveType::Personal).unwrap();
        sim.push_fault(Fault::delay(Duration::from_secs(30)));
        let started = tokio::time::Instant::now();
        get_json(&sim, &format!("{}/drives/{drive}", sim.base_url())).await;
        assert!(started.elapsed() >= Duration::from_secs(30));
        assert_eq!(sim.pending_faults(), 0);
    }

    /// Runs one fixed fault script and returns the status of each request.
    async fn run_script() -> Vec<u16> {
        let sim = GraphSim::start().await.unwrap();
        let drive = sim.add_drive(DriveType::Business).unwrap();
        sim.push_fault(Fault::status(StatusCode::TOO_MANY_REQUESTS).retry_after(1));
        sim.push_fault(Fault::status(StatusCode::INTERNAL_SERVER_ERROR).on_path("/delta"));
        sim.push_fault(Fault::status(StatusCode::SERVICE_UNAVAILABLE).on_path("/drives/"));
        let urls = [
            format!("{}/drives/{drive}", sim.base_url()),
            format!("{}/drives/{drive}", sim.base_url()),
            delta_url(&sim, &drive),
            delta_url(&sim, &drive),
            format!("{}/drives/{drive}", sim.base_url()),
        ];
        let mut statuses = Vec::new();
        for url in urls {
            let response = client()
                .get(url)
                .bearer_auth(sim.token())
                .send()
                .await
                .unwrap();
            statuses.push(response.status().as_u16());
        }
        statuses
    }

    /// ADR-0017 R5: fault injection is scriptable per test and deterministic for a given script.
    #[tokio::test]
    async fn adr_0017_r5_fault_script_is_deterministic() {
        let first = run_script().await;
        assert_eq!(first, vec![429, 503, 500, 200, 200]);
        assert_eq!(run_script().await, first);
    }

    /// AC7: two simulators run in one process without interfering: separate trees, tokens,
    /// faults and logs.
    #[tokio::test]
    async fn ac7_two_simulators_do_not_interfere() {
        let one = GraphSim::start().await.unwrap();
        let two = GraphSim::start().await.unwrap();
        assert_ne!(one.base_url(), two.base_url());
        assert_ne!(one.token(), two.token());

        let d1 = one.add_drive(DriveType::Personal).unwrap();
        let d2 = two.add_drive(DriveType::Personal).unwrap();
        let r1 = one.root_id(&d1).unwrap();
        one.create_file(&d1, &r1, "only-in-one", b"1".to_vec())
            .unwrap();

        one.push_fault(Fault::status(StatusCode::SERVICE_UNAVAILABLE));
        let (pages, _) = full_delta(&two, &delta_url(&two, &d2)).await;
        assert_eq!(items(&pages).len(), 1, "two has only its root");
        assert_eq!(
            one.pending_faults(),
            1,
            "two's request did not consume one's fault"
        );

        let cross = client()
            .get(format!("{}/drives/{d2}", two.base_url()))
            .bearer_auth(one.token())
            .send()
            .await
            .unwrap();
        assert_eq!(cross.status(), StatusCode::UNAUTHORIZED);

        one.clear_faults();
        let (pages, _) = full_delta(&one, &delta_url(&one, &d1)).await;
        let names: Vec<_> = items(&pages)
            .iter()
            .map(|i| i["name"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(names, vec!["root", "only-in-one"]);
        assert!(one.requests().iter().all(|r| r.origin == Origin::Graph));
        assert_eq!(two.requests().len(), 2);
    }

    /// The request log records method, path, query, headers of interest and status, and which
    /// origin served each request, so a test can check that no token reached the download host.
    #[tokio::test]
    async fn request_log_records_requests() {
        let sim = GraphSim::start().await.unwrap();
        let drive = sim.add_drive(DriveType::Personal).unwrap();
        let root = sim.root_id(&drive).unwrap();
        let file = sim
            .create_file(&drive, &root, "f", b"abc".to_vec())
            .unwrap();

        // reqwest's default policy follows the redirect and drops Authorization for the other
        // origin; the log shows what each origin received.
        let following = reqwest::Client::new();
        let response = following
            .get(format!(
                "{}/drives/{drive}/items/{file}/content",
                sim.base_url()
            ))
            .bearer_auth(sim.token())
            .header("User-Agent", "ISV|Example|my-drive-test/0.0")
            .header("Range", "bytes=0-0")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);

        let log = sim.requests();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].origin, Origin::Graph);
        assert_eq!(log[0].method, "GET");
        assert_eq!(
            log[0].path,
            format!("/v1.0/drives/{drive}/items/{file}/content")
        );
        assert_eq!(log[0].status, 302);
        let bearer = format!("Bearer {}", sim.token());
        assert_eq!(log[0].header("authorization"), Some(bearer.as_str()));
        assert_eq!(
            log[0].header("user-agent"),
            Some("ISV|Example|my-drive-test/0.0")
        );
        assert_eq!(log[1].origin, Origin::Download);
        assert_eq!(log[1].path, "/download");
        assert!(log[1].query.as_deref().unwrap().starts_with("tempauth="));
        assert_eq!(log[1].header("range"), Some("bytes=0-0"));
        assert_eq!(log[1].header("authorization"), None);
        assert_eq!(log[1].status, 206);

        sim.clear_requests();
        assert!(sim.requests().is_empty());
    }

    /// ADR-0017 R2: the simulator listens only on the loopback interface, so tests that use it
    /// make no request outside the machine.
    #[tokio::test]
    async fn adr_0017_r2_simulator_is_loopback_only() {
        let sim = GraphSim::start().await.unwrap();
        assert!(sim.graph_addr().ip().is_loopback());
        assert!(sim.base_url().starts_with("http://127.0.0.1:"));
        assert!(sim.download_origin().starts_with("http://127.0.0.1:"));
    }

    /// AC8: the README lists what is simulated with its source, and what is not simulated.
    #[test]
    fn ac8_readme_lists_simulated_and_not_simulated_behaviour() {
        let readme = include_str!("../README.md");
        assert!(readme.contains("## Simulated"));
        assert!(readme.contains("## Not simulated yet"));
        assert!(readme.contains("graph_api_and_windows_parity.md"));
    }
}

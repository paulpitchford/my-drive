//! Self-tests for the simulator's write side (WP-019), using a plain HTTP client.
//!
//! Each acceptance criterion has a test named `acN_...`. Every refused write is checked
//! to change nothing at all: no eTag bump, no delta entry, no partial content.

// The helpers below unwrap like the tests do; `cfg(test)` tells clippy they are test code
// (`allow-unwrap-in-tests` in `clippy.toml`).
#[cfg(test)]
mod write_self_tests {
    use mydrive_core::hash::QuickXorHasher;
    use mydrive_core::ids::{DriveId, ItemId};
    use mydrive_graph_sim::{
        ConflictBehavior, DeltaRepeats, DriveOptions, DriveType, FolderDeletes, GraphSim, Method,
        ReplaceStatus, RewriteMode, StatusCode,
    };
    use reqwest::redirect::Policy;
    use serde_json::{Value, json};

    /// The most pages a paging loop follows before the test fails instead of hanging.
    const MAX_PAGES: usize = 1000;

    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .redirect(Policy::none())
            .build()
            .unwrap()
    }

    fn quick_xor(bytes: &[u8]) -> String {
        let mut hasher = QuickXorHasher::new();
        hasher.update(bytes);
        hasher.finalize().to_base64()
    }

    /// One request with the simulator's token. Returns the status and the body as JSON
    /// (`Null` when empty or not JSON).
    async fn send(
        sim: &GraphSim,
        method: Method,
        url: &str,
        if_match: Option<&str>,
        body: Option<Vec<u8>>,
    ) -> (StatusCode, Value) {
        let mut request = client().request(method, url).bearer_auth(sim.token());
        if let Some(tag) = if_match {
            request = request.header("If-Match", tag);
        }
        if let Some(body) = body {
            request = request.body(body);
        }
        let response = request.send().await.unwrap();
        let status = response.status();
        let bytes = response.bytes().await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn get_json(sim: &GraphSim, url: &str) -> Value {
        let (status, body) = send(sim, Method::GET, url, None, None).await;
        assert_eq!(status, StatusCode::OK, "GET {url}: {body}");
        body
    }

    fn drive_url(sim: &GraphSim, drive: &DriveId) -> String {
        format!("{}/drives/{drive}", sim.base_url())
    }

    fn item_url(sim: &GraphSim, drive: &DriveId, item: &ItemId) -> String {
        format!("{}/items/{item}", drive_url(sim, drive))
    }

    async fn upload_new(
        sim: &GraphSim,
        drive: &DriveId,
        parent: &ItemId,
        name: &str,
        conflict: Option<&str>,
        content: &[u8],
    ) -> (StatusCode, Value) {
        let mut url = format!("{}/items/{parent}:/{name}:/content", drive_url(sim, drive));
        if let Some(conflict) = conflict {
            url.push_str(&format!("?@microsoft.graph.conflictBehavior={conflict}"));
        }
        send(sim, Method::PUT, &url, None, Some(content.to_vec())).await
    }

    async fn replace(
        sim: &GraphSim,
        drive: &DriveId,
        item: &ItemId,
        if_match: Option<&str>,
        content: &[u8],
    ) -> (StatusCode, Value) {
        let url = format!("{}/content", item_url(sim, drive, item));
        send(sim, Method::PUT, &url, if_match, Some(content.to_vec())).await
    }

    async fn patch(
        sim: &GraphSim,
        drive: &DriveId,
        item: &ItemId,
        if_match: Option<&str>,
        body: Value,
    ) -> (StatusCode, Value) {
        let url = item_url(sim, drive, item);
        let body = serde_json::to_vec(&body).unwrap();
        send(sim, Method::PATCH, &url, if_match, Some(body)).await
    }

    async fn delete(
        sim: &GraphSim,
        drive: &DriveId,
        item: &ItemId,
        if_match: Option<&str>,
    ) -> StatusCode {
        let url = item_url(sim, drive, item);
        send(sim, Method::DELETE, &url, if_match, None).await.0
    }

    async fn create_folder(
        sim: &GraphSim,
        drive: &DriveId,
        parent: &ItemId,
        body: Value,
    ) -> (StatusCode, Value) {
        let url = format!("{}/children", item_url(sim, drive, parent));
        let body = serde_json::to_vec(&body).unwrap();
        send(sim, Method::POST, &url, None, Some(body)).await
    }

    fn id_of(item: &Value) -> ItemId {
        ItemId::new(item["id"].as_str().unwrap()).unwrap()
    }

    /// Follows nextLinks from `url` to the deltaLink. Returns every item and the link.
    async fn full_delta(sim: &GraphSim, url: &str) -> (Vec<Value>, String) {
        let mut items = Vec::new();
        let mut next = url.to_owned();
        for _ in 0..MAX_PAGES {
            let page = get_json(sim, &next).await;
            items.extend(page["value"].as_array().unwrap().iter().cloned());
            match (
                page["@odata.nextLink"].as_str(),
                page["@odata.deltaLink"].as_str(),
            ) {
                (Some(link), None) => next = link.to_owned(),
                (None, Some(link)) => return (items, link.to_owned()),
                other => panic!("a page must carry exactly one link: {other:?}"),
            }
        }
        panic!("delta from {url} did not reach a deltaLink within {MAX_PAGES} pages");
    }

    async fn enumerate(sim: &GraphSim, drive: &DriveId) -> (Vec<Value>, String) {
        full_delta(sim, &format!("{}/root/delta", drive_url(sim, drive))).await
    }

    /// The whole content of a file, following the redirect if there is one.
    async fn download(sim: &GraphSim, drive: &DriveId, item: &ItemId) -> Vec<u8> {
        let url = format!("{}/content", item_url(sim, drive, item));
        let response = client()
            .get(&url)
            .bearer_auth(sim.token())
            .send()
            .await
            .unwrap();
        let response = if response.status() == StatusCode::FOUND {
            let location = response.headers()["location"].to_str().unwrap().to_owned();
            client().get(location).send().await.unwrap()
        } else {
            response
        };
        assert_eq!(response.status(), StatusCode::OK);
        response.bytes().await.unwrap().to_vec()
    }

    /// Everything a refused write could have touched: every live item as the token-less
    /// enumeration shows it (eTag, cTag, size, hash, names, parents, times), the drive's
    /// quota, the content of `watched`, and a delta link to check nothing new appears.
    struct Snapshot {
        items: Vec<Value>,
        used: Value,
        contents: Vec<Vec<u8>>,
        link: String,
    }

    async fn snapshot(sim: &GraphSim, drive: &DriveId, watched: &[&ItemId]) -> Snapshot {
        let (items, link) = enumerate(sim, drive).await;
        let used = get_json(sim, &drive_url(sim, drive)).await["quota"]["used"].clone();
        let mut contents = Vec::new();
        for item in watched {
            contents.push(download(sim, drive, item).await);
        }
        Snapshot {
            items,
            used,
            contents,
            link,
        }
    }

    async fn assert_unchanged(
        sim: &GraphSim,
        drive: &DriveId,
        watched: &[&ItemId],
        before: &Snapshot,
    ) {
        let (changes, _) = full_delta(sim, &before.link).await;
        assert!(
            changes.is_empty(),
            "a refused write reached delta: {changes:?}"
        );
        let after = snapshot(sim, drive, watched).await;
        assert_eq!(after.items, before.items, "a refused write changed an item");
        assert_eq!(after.used, before.used, "a refused write changed the quota");
        assert_eq!(
            after.contents, before.contents,
            "a refused write changed content"
        );
    }

    async fn sim_with(drive_type: DriveType, options: DriveOptions) -> (GraphSim, DriveId, ItemId) {
        let sim = GraphSim::start().await.unwrap();
        let drive = sim.add_drive_with(drive_type, options).unwrap();
        let root = sim.root_id(&drive).unwrap();
        (sim, drive, root)
    }

    fn honouring() -> DriveOptions {
        DriveOptions {
            simple_replace_honours_if_match: true,
            ..DriveOptions::default()
        }
    }

    /// AC1: on a route that honours `If-Match` (simple replace with the switch on, and
    /// update), a stale eTag answers `412` and changes nothing; the current eTag succeeds
    /// and the eTag and cTag change. Supports clients implementing ADR-0012 R6.
    #[tokio::test]
    async fn ac1_stale_if_match_is_412_and_current_etag_succeeds() {
        for drive_type in [DriveType::Personal, DriveType::Business] {
            let (sim, drive, root) = sim_with(drive_type, honouring()).await;
            let file = sim
                .create_file(&drive, &root, "f.txt", b"one".to_vec())
                .unwrap();
            let original = get_json(&sim, &item_url(&sim, &drive, &file)).await;
            let stale = original["eTag"].as_str().unwrap().to_owned();
            sim.modify_file(&drive, &file, b"two".to_vec()).unwrap();
            let current = get_json(&sim, &item_url(&sim, &drive, &file)).await;

            let before = snapshot(&sim, &drive, &[&file]).await;
            let (status, body) = replace(&sim, &drive, &file, Some(&stale), b"three").await;
            assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{drive_type:?}");
            assert_eq!(body["error"]["code"], "resourceModified");
            let (status, _) =
                patch(&sim, &drive, &file, Some(&stale), json!({"name": "g.txt"})).await;
            assert_eq!(status, StatusCode::PRECONDITION_FAILED);
            assert_unchanged(&sim, &drive, &[&file], &before).await;

            let etag = current["eTag"].as_str().unwrap();
            let (status, replaced) = replace(&sim, &drive, &file, Some(etag), b"three").await;
            assert_eq!(status, StatusCode::CREATED);
            assert_eq!(replaced["id"], file.as_str());
            assert_eq!(replaced["size"], 5);
            assert_eq!(
                replaced["file"]["hashes"]["quickXorHash"],
                quick_xor(b"three")
            );
            assert_ne!(replaced["eTag"], current["eTag"]);
            assert_ne!(replaced["cTag"], current["cTag"]);
            assert_eq!(download(&sim, &drive, &file).await, b"three");

            // The cTag is accepted too ("the eTag (or cTag)"), and a rename keeps it.
            let ctag = replaced["cTag"].as_str().unwrap();
            let (status, renamed) =
                patch(&sim, &drive, &file, Some(ctag), json!({"name": "g.txt"})).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(renamed["name"], "g.txt");
            assert_ne!(renamed["eTag"], replaced["eTag"]);
            assert_eq!(renamed["cTag"], replaced["cTag"]);
        }
    }

    /// By default a simple replace does not check `If-Match` (unverified, so the harder
    /// case): a stale eTag overwrites. The header is still logged.
    #[tokio::test]
    async fn simple_replace_ignores_if_match_by_default() {
        let (sim, drive, root) = sim_with(DriveType::Personal, DriveOptions::default()).await;
        let file = sim
            .create_file(&drive, &root, "f", b"one".to_vec())
            .unwrap();
        let (status, item) = replace(&sim, &drive, &file, Some("\"{stale},1\""), b"two").await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(item["size"], 3);
        assert_eq!(download(&sim, &drive, &file).await, b"two");
        // The create-by-name form replacing an existing file ignores it too.
        let (status, item) = upload_new(&sim, &drive, &root, "f", None, b"three").await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(item["id"], file.as_str());
    }

    /// With the switch on, the create-by-name form checks `If-Match` against the file it
    /// would replace, and a create with `If-Match` where no item exists fails.
    #[tokio::test]
    async fn create_by_name_checks_if_match_when_honoured() {
        let (sim, drive, root) = sim_with(DriveType::Business, honouring()).await;
        let file = sim
            .create_file(&drive, &root, "f", b"one".to_vec())
            .unwrap();
        let before = snapshot(&sim, &drive, &[&file]).await;
        for name in ["f", "absent"] {
            let url = format!("{}/items/{root}:/{name}:/content", drive_url(&sim, &drive));
            let (status, _) = send(
                &sim,
                Method::PUT,
                &url,
                Some("\"{x},1\""),
                Some(b"zz".to_vec()),
            )
            .await;
            assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{name}");
        }
        assert_unchanged(&sim, &drive, &[&file], &before).await;
        let etag = before
            .items
            .iter()
            .find(|i| i["id"] == file.as_str())
            .unwrap()["eTag"]
            .as_str()
            .unwrap()
            .to_owned();
        let url = format!("{}/items/{root}:/f:/content", drive_url(&sim, &drive));
        let (status, _) = send(&sim, Method::PUT, &url, Some(&etag), Some(b"zz".to_vec())).await;
        assert_eq!(status, StatusCode::CREATED);
    }

    /// AC2: a create with `conflictBehavior=fail` on a name that differs only in case
    /// answers `409 nameAlreadyExists` and creates nothing, for a file upload and a
    /// folder; `rename` creates `<stem> 1.<ext>`; the PUT default `replace` keeps the ID.
    #[tokio::test]
    async fn ac2_fail_on_case_variant_is_409_and_creates_nothing() {
        let (sim, drive, root) = sim_with(DriveType::Personal, DriveOptions::default()).await;
        let file = sim
            .create_file(&drive, &root, "hello.doc", b"x".to_vec())
            .unwrap();
        let folder = sim.create_folder(&drive, &root, "Docs").unwrap();

        let before = snapshot(&sim, &drive, &[&file]).await;
        let (status, body) =
            upload_new(&sim, &drive, &root, "HELLO.doc", Some("fail"), b"new").await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"]["code"], "nameAlreadyExists");
        let (status, body) = create_folder(
            &sim,
            &drive,
            &root,
            json!({"name": "docs", "folder": {}, "@microsoft.graph.conflictBehavior": "fail"}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"]["code"], "nameAlreadyExists");
        // A folder in the way of a file upload is a conflict even under `replace`.
        let (status, _) = upload_new(&sim, &drive, &root, "DOCS", Some("replace"), b"n").await;
        assert_eq!(status, StatusCode::CONFLICT);
        // Renaming onto a sibling's name, in another case, is a conflict too.
        let (status, body) = patch(&sim, &drive, &folder, None, json!({"name": "Hello.DOC"})).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"]["code"], "nameAlreadyExists");
        assert_unchanged(&sim, &drive, &[&file], &before).await;

        let (status, renamed) =
            upload_new(&sim, &drive, &root, "HELLO.doc", Some("rename"), b"new").await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(renamed["name"], "HELLO 1.doc");
        assert_ne!(renamed["id"], file.as_str());

        let (status, replaced) = upload_new(&sim, &drive, &root, "Hello.Doc", None, b"rep").await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(replaced["id"], file.as_str());
        assert_eq!(
            replaced["name"], "hello.doc",
            "a replace keeps the stored name"
        );
        assert_eq!(download(&sim, &drive, &file).await, b"rep");
    }

    /// One change, made either way.
    #[derive(Debug, Clone, Copy)]
    enum Step {
        CreateFolder,
        Upload,
        Replace,
        ReplaceByName,
        Rename,
        Move,
        RenameFolder,
        DeleteFile,
        DeleteFolder,
    }

    const STEPS: [Step; 9] = [
        Step::CreateFolder,
        Step::Upload,
        Step::Replace,
        Step::ReplaceByName,
        Step::Rename,
        Step::Move,
        Step::RenameFolder,
        Step::DeleteFile,
        Step::DeleteFolder,
    ];

    /// The tree both simulators start from, built through the control API.
    struct Tree {
        root: ItemId,
        dir: ItemId,
        old: ItemId,
        keep: ItemId,
        new_folder: Option<ItemId>,
        uploaded: Option<ItemId>,
    }

    fn seed(sim: &GraphSim, drive: &DriveId) -> Tree {
        let root = sim.root_id(drive).unwrap();
        let dir = sim.create_folder(drive, &root, "Dir").unwrap();
        let old = sim
            .create_file(drive, &dir, "old.txt", b"old".to_vec())
            .unwrap();
        let keep = sim
            .create_file(drive, &root, "keep.txt", b"keep".to_vec())
            .unwrap();
        Tree {
            root,
            dir,
            old,
            keep,
            new_folder: None,
            uploaded: None,
        }
    }

    /// Applies `step` through the control API.
    fn by_control(sim: &GraphSim, drive: &DriveId, tree: &mut Tree, step: Step) {
        match step {
            Step::CreateFolder => {
                tree.new_folder = Some(sim.create_folder(drive, &tree.dir, "New").unwrap());
            }
            Step::Upload => {
                tree.uploaded = Some(
                    sim.create_file(drive, &tree.dir, "n.txt", b"new file".to_vec())
                        .unwrap(),
                );
            }
            Step::Replace => sim
                .modify_file(drive, &tree.old, b"replaced!".to_vec())
                .unwrap(),
            Step::ReplaceByName => sim
                .modify_file(drive, &tree.old, b"replaced by name".to_vec())
                .unwrap(),
            Step::Rename => sim.rename(drive, &tree.keep, "kept.txt").unwrap(),
            Step::Move => sim
                .move_item(drive, &tree.keep, tree.new_folder.as_ref().unwrap())
                .unwrap(),
            Step::RenameFolder => sim.rename(drive, &tree.dir, "Dir2").unwrap(),
            Step::DeleteFile => sim.delete(drive, tree.uploaded.as_ref().unwrap()).unwrap(),
            Step::DeleteFolder => sim.delete(drive, &tree.dir).unwrap(),
        }
    }

    /// Applies `step` through HTTP, checking it succeeded and gave the same IDs.
    async fn by_http(sim: &GraphSim, drive: &DriveId, tree: &mut Tree, step: Step) {
        match step {
            Step::CreateFolder => {
                let (status, item) = create_folder(
                    sim,
                    drive,
                    &tree.dir,
                    json!({"name": "New", "folder": {}, "@microsoft.graph.conflictBehavior": "fail"}),
                )
                .await;
                assert_eq!(status, StatusCode::CREATED);
                tree.new_folder = Some(id_of(&item));
            }
            Step::Upload => {
                let (status, item) =
                    upload_new(sim, drive, &tree.dir, "n.txt", Some("fail"), b"new file").await;
                assert_eq!(status, StatusCode::CREATED);
                tree.uploaded = Some(id_of(&item));
            }
            Step::Replace => {
                let (status, _) = replace(sim, drive, &tree.old, None, b"replaced!").await;
                assert_eq!(status, StatusCode::CREATED);
            }
            Step::ReplaceByName => {
                let (status, item) = upload_new(
                    sim,
                    drive,
                    &tree.dir,
                    "OLD.TXT",
                    Some("replace"),
                    b"replaced by name",
                )
                .await;
                assert_eq!(status, StatusCode::CREATED);
                assert_eq!(id_of(&item), tree.old);
            }
            Step::Rename => {
                let (status, _) =
                    patch(sim, drive, &tree.keep, None, json!({"name": "kept.txt"})).await;
                assert_eq!(status, StatusCode::OK);
            }
            Step::Move => {
                let target = tree.new_folder.as_ref().unwrap().as_str();
                let (status, _) = patch(
                    sim,
                    drive,
                    &tree.keep,
                    None,
                    json!({"parentReference": {"id": target}}),
                )
                .await;
                assert_eq!(status, StatusCode::OK);
            }
            Step::RenameFolder => {
                let (status, _) = patch(sim, drive, &tree.dir, None, json!({"name": "Dir2"})).await;
                assert_eq!(status, StatusCode::OK);
            }
            Step::DeleteFile => {
                let status = delete(sim, drive, tree.uploaded.as_ref().unwrap(), None).await;
                assert_eq!(status, StatusCode::NO_CONTENT);
            }
            Step::DeleteFolder => {
                assert_eq!(
                    delete(sim, drive, &tree.dir, None).await,
                    StatusCode::NO_CONTENT
                );
            }
        }
    }

    /// AC3: every write made through HTTP appears in delta exactly as the same change
    /// made through the control API: the same entries in the same order with the same
    /// eTags, cTags (folder cTags under both settings of `personal_folder_ctag`), sizes,
    /// hashes and times, in both repeat modes and both folder-delete modes, on personal
    /// and business drives. Each change is compared alone, then the whole run from the
    /// first token and the final enumeration. Supports clients implementing ADR-0011 R2.
    #[tokio::test]
    async fn ac3_http_writes_look_like_control_api_changes_in_delta() {
        let cases = [
            (DriveType::Personal, false),
            (DriveType::Personal, true),
            (DriveType::Business, false),
            (DriveType::DocumentLibrary, false),
        ];
        for (drive_type, personal_folder_ctag) in cases {
            for delta_repeats in [DeltaRepeats::EveryChange, DeltaRepeats::LatestStateOnce] {
                for folder_deletes in [FolderDeletes::FolderOnly, FolderDeletes::EveryDescendant] {
                    let label = format!(
                        "{drive_type:?} folder cTag {personal_folder_ctag} {delta_repeats:?} {folder_deletes:?}"
                    );
                    let options = DriveOptions {
                        delta_repeats,
                        folder_deletes,
                        personal_folder_ctag,
                        delta_page_size: 3,
                        ..DriveOptions::default()
                    };
                    // One simulator per side, each with one drive, so IDs and eTags are
                    // the same on both sides and the feeds compare byte for byte.
                    let (control, drive_c, _) = sim_with(drive_type, options.clone()).await;
                    let (http, drive_h, _) = sim_with(drive_type, options).await;
                    assert_eq!(drive_c, drive_h);
                    let mut tree_c = seed(&control, &drive_c);
                    let mut tree_h = seed(&http, &drive_h);
                    let (_, first_c) = enumerate(&control, &drive_c).await;
                    let (_, first_h) = enumerate(&http, &drive_h).await;
                    let (mut link_c, mut link_h) = (first_c.clone(), first_h.clone());
                    for step in STEPS {
                        control.advance_clock(7);
                        http.advance_clock(7);
                        by_control(&control, &drive_c, &mut tree_c, step);
                        by_http(&http, &drive_h, &mut tree_h, step).await;
                        assert_eq!(tree_c.new_folder, tree_h.new_folder, "{label}");
                        assert_eq!(tree_c.uploaded, tree_h.uploaded, "{label}");
                        let (feed_c, next_c) = full_delta(&control, &link_c).await;
                        let (feed_h, next_h) = full_delta(&http, &link_h).await;
                        assert!(!feed_c.is_empty(), "{label} {step:?}");
                        assert_eq!(feed_h, feed_c, "{label} {step:?}");
                        (link_c, link_h) = (next_c, next_h);
                    }
                    let (all_c, _) = full_delta(&control, &first_c).await;
                    let (all_h, _) = full_delta(&http, &first_h).await;
                    assert_eq!(all_h, all_c, "{label}: the whole run");
                    let (now_c, _) = enumerate(&control, &drive_c).await;
                    let (now_h, _) = enumerate(&http, &drive_h).await;
                    assert_eq!(now_h, now_c, "{label}: enumeration");
                    assert_eq!(tree_c.root, tree_h.root);
                }
            }
        }
    }

    /// A PATCH that renames and moves at once is one change: under `EveryChange` the item
    /// appears once in the next delta.
    #[tokio::test]
    async fn rename_and_move_in_one_patch_is_one_change() {
        let (sim, drive, root) = sim_with(DriveType::Personal, DriveOptions::default()).await;
        let folder = sim.create_folder(&drive, &root, "To").unwrap();
        let file = sim.create_file(&drive, &root, "a", b"1".to_vec()).unwrap();
        let (_, link) = enumerate(&sim, &drive).await;
        let (status, item) = patch(
            &sim,
            &drive,
            &file,
            None,
            json!({"name": "b", "parentReference": {"id": folder.as_str(), "driveId": drive.as_str()}}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(item["name"], "b");
        assert_eq!(item["parentReference"]["id"], folder.as_str());
        assert_eq!(item["parentReference"]["path"], "/drive/root:/To");
        let (changes, _) = full_delta(&sim, &link).await;
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0]["id"], file.as_str());
    }

    /// AC4: with rewriting on, a library serves a hash and size afterwards that differ
    /// from the uploaded bytes', downloads return the rewritten bytes, and hash, size and
    /// bytes agree with each other. In `Later` mode (the default) the upload response
    /// still reports the uploaded bytes' hash and an eTag that is already stale; in
    /// `InResponse` mode the response is already rewritten. Personal and business drives,
    /// unlisted extensions and `Off` are never rewritten. Supports clients implementing
    /// ADR-0012 R8.
    #[tokio::test]
    async fn ac4_library_rewrite_changes_hash_size_and_bytes() {
        let uploaded = b"PK\x03\x04 pretend this is a document".to_vec();
        for mode in [RewriteMode::Later, RewriteMode::InResponse] {
            let options = DriveOptions {
                rewrite: mode,
                ..DriveOptions::default()
            };
            let (sim, drive, root) = sim_with(DriveType::DocumentLibrary, options).await;
            let (status, response) =
                upload_new(&sim, &drive, &root, "Report.DOCX", Some("fail"), &uploaded).await;
            assert_eq!(status, StatusCode::CREATED, "{mode:?}");
            let file = id_of(&response);
            let served = get_json(&sim, &item_url(&sim, &drive, &file)).await;
            let bytes = download(&sim, &drive, &file).await;

            assert_ne!(bytes, uploaded, "{mode:?}");
            assert_eq!(served["size"], bytes.len(), "{mode:?}");
            assert_eq!(served["file"]["hashes"]["quickXorHash"], quick_xor(&bytes));
            assert_ne!(served["size"], uploaded.len());
            assert_ne!(
                served["file"]["hashes"]["quickXorHash"],
                quick_xor(&uploaded)
            );
            match mode {
                RewriteMode::Later => {
                    assert_eq!(response["size"], uploaded.len());
                    assert_eq!(
                        response["file"]["hashes"]["quickXorHash"],
                        quick_xor(&uploaded)
                    );
                    assert_ne!(response["eTag"], served["eTag"]);
                    assert_ne!(response["cTag"], served["cTag"]);
                }
                _ => {
                    assert_eq!(response["size"], served["size"]);
                    assert_eq!(response["file"]["hashes"], served["file"]["hashes"]);
                    assert_eq!(response["eTag"], served["eTag"]);
                }
            }
            let (feed, _) = enumerate(&sim, &drive).await;
            let in_delta = feed.iter().find(|i| i["id"] == file.as_str()).unwrap();
            assert_eq!(in_delta["file"]["hashes"], served["file"]["hashes"]);
            assert_eq!(in_delta["size"], served["size"]);

            // A replace of a listed file is rewritten too, and the quota counts what is
            // stored.
            let (status, _) = replace(&sim, &drive, &file, None, b"v2").await;
            assert_eq!(status, StatusCode::CREATED);
            let bytes = download(&sim, &drive, &file).await;
            assert_ne!(bytes, b"v2");
            let served = get_json(&sim, &item_url(&sim, &drive, &file)).await;
            assert_eq!(served["file"]["hashes"]["quickXorHash"], quick_xor(&bytes));
            let quota = get_json(&sim, &drive_url(&sim, &drive)).await["quota"]["used"].clone();
            assert_eq!(quota, bytes.len());
        }

        // Not rewritten: an unlisted extension, `Off`, and drives that are not libraries.
        let off = DriveOptions {
            rewrite: RewriteMode::Off,
            ..DriveOptions::default()
        };
        for (drive_type, options, name) in [
            (
                DriveType::DocumentLibrary,
                DriveOptions::default(),
                "notes.txt",
            ),
            (DriveType::DocumentLibrary, off, "a.docx"),
            (DriveType::Business, DriveOptions::default(), "a.docx"),
            (DriveType::Personal, DriveOptions::default(), "a.docx"),
        ] {
            let (sim, drive, root) = sim_with(drive_type, options).await;
            let (status, response) = upload_new(&sim, &drive, &root, name, None, &uploaded).await;
            assert_eq!(status, StatusCode::CREATED);
            let file = id_of(&response);
            assert_eq!(
                download(&sim, &drive, &file).await,
                uploaded,
                "{drive_type:?} {name}"
            );
            let served = get_json(&sim, &item_url(&sim, &drive, &file)).await;
            assert_eq!(
                served["file"]["hashes"]["quickXorHash"],
                quick_xor(&uploaded)
            );
            assert_eq!(served["eTag"], response["eTag"]);
        }
    }

    /// AC5: a delete with a stale `If-Match` answers `412` and deletes nothing; with the
    /// current eTag it answers `204`, and the item then appears in delta with the
    /// `deleted` facet, its descendants following the folder-delete switch. A second
    /// delete answers `404`.
    #[tokio::test]
    async fn ac5_delete_checks_if_match_and_appears_deleted_in_delta() {
        for folder_deletes in [FolderDeletes::FolderOnly, FolderDeletes::EveryDescendant] {
            let options = DriveOptions {
                folder_deletes,
                ..DriveOptions::default()
            };
            let (sim, drive, root) = sim_with(DriveType::Business, options).await;
            let dir = sim.create_folder(&drive, &root, "Dir").unwrap();
            let inner = sim
                .create_file(&drive, &dir, "in.txt", b"in".to_vec())
                .unwrap();
            let stale = get_json(&sim, &item_url(&sim, &drive, &dir)).await["eTag"]
                .as_str()
                .unwrap()
                .to_owned();
            sim.rename(&drive, &dir, "Dir2").unwrap();

            let before = snapshot(&sim, &drive, &[&inner]).await;
            assert_eq!(
                delete(&sim, &drive, &dir, Some(&stale)).await,
                StatusCode::PRECONDITION_FAILED
            );
            assert_unchanged(&sim, &drive, &[&inner], &before).await;

            let current = before
                .items
                .iter()
                .find(|i| i["id"] == dir.as_str())
                .unwrap()["eTag"]
                .as_str()
                .unwrap()
                .to_owned();
            assert_eq!(
                delete(&sim, &drive, &dir, Some(&current)).await,
                StatusCode::NO_CONTENT
            );
            let (changes, _) = full_delta(&sim, &before.link).await;
            assert!(changes.iter().all(|i| i["deleted"].is_object()));
            let ids: Vec<&str> = changes.iter().map(|i| i["id"].as_str().unwrap()).collect();
            match folder_deletes {
                FolderDeletes::FolderOnly => assert_eq!(ids, vec![dir.as_str()]),
                FolderDeletes::EveryDescendant => {
                    assert_eq!(ids, vec![dir.as_str(), inner.as_str()]);
                }
            }
            assert_eq!(
                delete(&sim, &drive, &dir, None).await,
                StatusCode::NOT_FOUND
            );
            assert_eq!(
                delete(&sim, &drive, &inner, None).await,
                StatusCode::NOT_FOUND
            );
        }
    }

    /// AC6: the request log records the `if-match` header of every write, and its absence.
    #[tokio::test]
    async fn ac6_request_log_records_if_match_of_every_write() {
        let (sim, drive, root) = sim_with(DriveType::Personal, DriveOptions::default()).await;
        let file = sim.create_file(&drive, &root, "f", b"1".to_vec()).unwrap();
        let gone = sim.create_file(&drive, &root, "g", b"1".to_vec()).unwrap();
        sim.clear_requests();
        let tag = "\"{some-tag},7\"";
        replace(&sim, &drive, &file, Some(tag), b"2").await;
        let url = format!("{}/items/{root}:/h:/content", drive_url(&sim, &drive));
        send(&sim, Method::PUT, &url, Some(tag), Some(b"3".to_vec())).await;
        patch(&sim, &drive, &file, Some(tag), json!({"name": "f2"})).await;
        delete(&sim, &drive, &gone, Some(tag)).await;
        let url = format!("{}/children", item_url(&sim, &drive, &root));
        let body = serde_json::to_vec(&json!({"name": "D", "folder": {}})).unwrap();
        send(&sim, Method::POST, &url, Some(tag), Some(body)).await;
        replace(&sim, &drive, &file, None, b"4").await;

        let log = sim.requests();
        let methods: Vec<&str> = log.iter().map(|r| r.method.as_str()).collect();
        assert_eq!(
            methods,
            vec!["PUT", "PUT", "PATCH", "DELETE", "POST", "PUT"]
        );
        for request in &log[..5] {
            assert_eq!(request.header("if-match"), Some(tag), "{request:?}");
        }
        assert_eq!(log[5].header("if-match"), None);
        let statuses: Vec<u16> = log.iter().map(|r| r.status).collect();
        assert_eq!(statuses, vec![201, 201, 412, 412, 201, 201]);
    }

    /// AC7: a write that would take the drive past its quota answers `507
    /// quotaLimitReached` and stores nothing; up to the quota is fine, and replacing a
    /// file counts only the difference.
    #[tokio::test]
    async fn ac7_exceeding_the_quota_is_507_and_stores_nothing() {
        let options = DriveOptions {
            quota_bytes: 10,
            ..DriveOptions::default()
        };
        let (sim, drive, root) = sim_with(DriveType::Personal, options).await;
        let file = sim
            .create_file(&drive, &root, "six", b"123456".to_vec())
            .unwrap();
        let before = snapshot(&sim, &drive, &[&file]).await;
        let (status, body) = upload_new(&sim, &drive, &root, "five", None, b"12345").await;
        assert_eq!(status, StatusCode::INSUFFICIENT_STORAGE);
        assert_eq!(body["error"]["code"], "quotaLimitReached");
        let (status, _) = replace(&sim, &drive, &file, None, b"12345678901").await;
        assert_eq!(status, StatusCode::INSUFFICIENT_STORAGE);
        assert_unchanged(&sim, &drive, &[&file], &before).await;

        let (status, _) = upload_new(&sim, &drive, &root, "four", None, b"1234").await;
        assert_eq!(status, StatusCode::CREATED);
        let (status, _) = replace(&sim, &drive, &file, None, b"123").await;
        assert_eq!(status, StatusCode::CREATED);
        let used = get_json(&sim, &drive_url(&sim, &drive)).await["quota"]["used"].clone();
        assert_eq!(used, 7);
    }

    /// A locked item refuses a replace, an update and a delete with `423`, and so does a
    /// folder above it on delete; nothing changes. Unlocked, the same writes succeed.
    #[tokio::test]
    async fn locked_item_refuses_writes_with_423() {
        let (sim, drive, root) = sim_with(DriveType::Business, DriveOptions::default()).await;
        let dir = sim.create_folder(&drive, &root, "Dir").unwrap();
        let file = sim
            .create_file(&drive, &dir, "f.docx", b"1".to_vec())
            .unwrap();
        sim.set_locked(&drive, &file, true).unwrap();
        let before = snapshot(&sim, &drive, &[&file]).await;
        let (status, body) = replace(&sim, &drive, &file, None, b"2").await;
        assert_eq!(status, StatusCode::LOCKED);
        assert_eq!(body["error"]["code"], "resourceLocked");
        let (status, _) = upload_new(&sim, &drive, &dir, "F.docx", None, b"2").await;
        assert_eq!(status, StatusCode::LOCKED);
        let (status, _) = patch(&sim, &drive, &file, None, json!({"name": "g.docx"})).await;
        assert_eq!(status, StatusCode::LOCKED);
        assert_eq!(delete(&sim, &drive, &file, None).await, StatusCode::LOCKED);
        assert_eq!(delete(&sim, &drive, &dir, None).await, StatusCode::LOCKED);
        assert_unchanged(&sim, &drive, &[&file], &before).await;

        sim.set_locked(&drive, &file, false).unwrap();
        let (status, _) = replace(&sim, &drive, &file, None, b"2").await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(
            delete(&sim, &drive, &dir, None).await,
            StatusCode::NO_CONTENT
        );
        assert!(
            sim.set_locked(&drive, &file, true).is_err(),
            "deleted items cannot lock"
        );
    }

    /// Bad requests answer `400 invalidRequest` (or `404` for what does not exist) and
    /// change nothing: malformed JSON, a bad conflict behaviour, content for a folder, a
    /// name with `/`, a move into itself, a move to another drive, renaming the root.
    #[tokio::test]
    async fn bad_requests_are_400_and_change_nothing() {
        let (sim, drive, root) = sim_with(DriveType::Personal, DriveOptions::default()).await;
        let dir = sim.create_folder(&drive, &root, "Dir").unwrap();
        let sub = sim.create_folder(&drive, &dir, "Sub").unwrap();
        let file = sim.create_file(&drive, &dir, "f", b"1".to_vec()).unwrap();
        let before = snapshot(&sim, &drive, &[&file]).await;
        let item = item_url(&sim, &drive, &dir);

        let bad: Vec<(StatusCode, Value)> = vec![
            send(&sim, Method::PATCH, &item, None, Some(b"{not json".to_vec())).await,
            send(&sim, Method::PATCH, &item, None, Some(b"[]".to_vec())).await,
            patch(&sim, &drive, &dir, None, json!({"name": 5})).await,
            patch(&sim, &drive, &dir, None, json!({"name": ""})).await,
            patch(&sim, &drive, &dir, None, json!({"parentReference": {"id": sub.as_str()}})).await,
            patch(&sim, &drive, &dir, None, json!({"parentReference": {"id": dir.as_str()}})).await,
            patch(&sim, &drive, &dir, None, json!({"parentReference": {"path": "/drive/root:"}})).await,
            patch(
                &sim,
                &drive,
                &dir,
                None,
                json!({"parentReference": {"id": root.as_str(), "driveId": "0000000000000000"}}),
            )
            .await,
            patch(&sim, &drive, &dir, None, json!({"parentReference": {"id": file.as_str()}})).await,
            patch(&sim, &drive, &dir, None, json!({"fileSystemInfo": {"lastModifiedDateTime": "yesterday"}})).await,
            patch(&sim, &drive, &root, None, json!({"name": "top"})).await,
            replace(&sim, &drive, &dir, None, b"content for a folder").await,
            upload_new(&sim, &drive, &dir, "new", Some("sometimes"), b"x").await,
            upload_new(&sim, &drive, &dir, "a%2Fb", None, b"x").await,
            upload_new(&sim, &drive, &file, "child", None, b"x").await,
            create_folder(&sim, &drive, &dir, json!({"folder": {}})).await,
            create_folder(&sim, &drive, &dir, json!({"name": "N"})).await,
            create_folder(&sim, &drive, &dir, json!({"name": "Sub", "folder": {}, "@microsoft.graph.conflictBehavior": "replace"})).await,
            create_folder(&sim, &drive, &dir, json!({"name": "N", "folder": {}, "@microsoft.graph.conflictBehavior": 1})).await,
        ];
        for (n, (status, body)) in bad.iter().enumerate() {
            assert_eq!(*status, StatusCode::BAD_REQUEST, "case {n}: {body}");
            assert_eq!(body["error"]["code"], "invalidRequest", "case {n}");
        }
        assert_eq!(
            delete(&sim, &drive, &root, None).await,
            StatusCode::BAD_REQUEST
        );

        let missing = ItemId::new("NO-SUCH-ITEM").unwrap();
        let (status, _) = replace(&sim, &drive, &missing, None, b"x").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = upload_new(&sim, &drive, &missing, "x", None, b"x").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, body) = patch(
            &sim,
            &drive,
            &dir,
            None,
            json!({"parentReference": {"id": "NO-SUCH-ITEM"}}),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "itemNotFound");
        let (status, _) =
            create_folder(&sim, &drive, &missing, json!({"name": "x", "folder": {}})).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_unchanged(&sim, &drive, &[&file], &before).await;

        // Writes need the bearer token like every other Graph request.
        let response = client()
            .delete(item_url(&sim, &drive, &file))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_unchanged(&sim, &drive, &[&file], &before).await;
    }

    /// A simple upload larger than `simple_upload_max_bytes` answers `413` and stores
    /// nothing; one at the limit is stored. (A body without `Content-Length` is checked
    /// by a unit test in `write.rs`.)
    #[tokio::test]
    async fn simple_upload_over_the_limit_is_413() {
        let options = DriveOptions {
            simple_upload_max_bytes: 8,
            ..DriveOptions::default()
        };
        let (sim, drive, root) = sim_with(DriveType::Personal, options).await;
        let file = sim.create_file(&drive, &root, "f", b"1".to_vec()).unwrap();
        let before = snapshot(&sim, &drive, &[&file]).await;
        let (status, _) = upload_new(&sim, &drive, &root, "big", None, b"123456789").await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        let (status, _) = replace(&sim, &drive, &file, None, b"123456789").await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_unchanged(&sim, &drive, &[&file], &before).await;

        let (status, _) = upload_new(&sim, &drive, &root, "big", None, b"12345678").await;
        assert_eq!(status, StatusCode::CREATED);
    }

    /// `fileSystemInfo` times supplied when creating a folder or updating an item are kept,
    /// at whole seconds; a content replace without them resets `lastModifiedDateTime` to
    /// the time of the change. Supports clients implementing ADR-0011 R5.
    #[tokio::test]
    async fn file_system_info_is_kept_at_whole_seconds() {
        let (sim, drive, root) = sim_with(DriveType::Business, DriveOptions::default()).await;
        let (status, folder) = create_folder(
            &sim,
            &drive,
            &root,
            json!({
                "name": "Dated",
                "folder": {},
                "fileSystemInfo": {
                    "createdDateTime": "2020-05-06T07:08:09.987Z",
                    "lastModifiedDateTime": "2021-05-06T09:08:09+02:00",
                },
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(
            folder["fileSystemInfo"]["createdDateTime"],
            "2020-05-06T07:08:09Z"
        );
        assert_eq!(
            folder["fileSystemInfo"]["lastModifiedDateTime"],
            "2021-05-06T07:08:09Z"
        );

        let file = sim.create_file(&drive, &root, "f", b"1".to_vec()).unwrap();
        let ctag = get_json(&sim, &item_url(&sim, &drive, &file)).await["cTag"].clone();
        let (status, item) = patch(
            &sim,
            &drive,
            &file,
            None,
            json!({"fileSystemInfo": {"lastModifiedDateTime": "2019-12-31T23:59:59.999999Z"}}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            item["fileSystemInfo"]["lastModifiedDateTime"],
            "2019-12-31T23:59:59Z"
        );
        assert_eq!(
            item["fileSystemInfo"]["createdDateTime"],
            "2026-01-01T00:00:00Z"
        );
        let fetched = get_json(&sim, &item_url(&sim, &drive, &file)).await;
        assert_eq!(fetched["fileSystemInfo"], item["fileSystemInfo"]);
        assert_eq!(fetched["cTag"], ctag, "a metadata change keeps the cTag");

        sim.advance_clock(90);
        let (status, item) = replace(&sim, &drive, &file, None, b"2").await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(
            item["fileSystemInfo"]["lastModifiedDateTime"],
            "2026-01-01T00:01:30Z"
        );
    }

    /// `rename_sets_modified` decides whether a rename or move changes
    /// `lastModifiedDateTime`, through HTTP and the control API alike; by default it does
    /// not.
    #[tokio::test]
    async fn rename_sets_modified_follows_the_switch() {
        for on in [false, true] {
            let options = DriveOptions {
                rename_sets_modified: on,
                ..DriveOptions::default()
            };
            let (sim, drive, root) = sim_with(DriveType::Personal, options).await;
            let a = sim.create_file(&drive, &root, "a", b"1".to_vec()).unwrap();
            let b = sim.create_file(&drive, &root, "b", b"1".to_vec()).unwrap();
            sim.advance_clock(60);
            let (status, item) = patch(&sim, &drive, &a, None, json!({"name": "a2"})).await;
            assert_eq!(status, StatusCode::OK);
            sim.rename(&drive, &b, "b2").unwrap();
            let control = get_json(&sim, &item_url(&sim, &drive, &b)).await;
            let expected = if on {
                "2026-01-01T00:01:00Z"
            } else {
                "2026-01-01T00:00:00Z"
            };
            assert_eq!(item["lastModifiedDateTime"], expected, "switch {on}");
            assert_eq!(control["lastModifiedDateTime"], expected, "switch {on}");
        }
    }

    /// Create folder without a conflict behaviour follows `folder_conflict_default`
    /// (default `Rename`); the query string wins over the body.
    #[tokio::test]
    async fn create_folder_default_conflict_behaviour_follows_the_switch() {
        let (sim, drive, root) = sim_with(DriveType::Personal, DriveOptions::default()).await;
        sim.create_folder(&drive, &root, "New").unwrap();
        let (status, item) =
            create_folder(&sim, &drive, &root, json!({"name": "new", "folder": {}})).await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(item["name"], "new 1");
        assert_eq!(item["folder"]["childCount"], 0);

        let mut options = sim.options(&drive).unwrap();
        options.folder_conflict_default = ConflictBehavior::Fail;
        sim.set_options(&drive, options).unwrap();
        let (status, _) =
            create_folder(&sim, &drive, &root, json!({"name": "NEW", "folder": {}})).await;
        assert_eq!(status, StatusCode::CONFLICT);
        let url = format!(
            "{}/root/children?@microsoft.graph.conflictBehavior=rename",
            drive_url(&sim, &drive)
        );
        let body =
            json!({"name": "NEW", "folder": {}, "@microsoft.graph.conflictBehavior": "fail"});
        let (status, item) = send(
            &sim,
            Method::POST,
            &url,
            None,
            Some(serde_json::to_vec(&body).unwrap()),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(item["name"], "NEW 2");
    }

    /// AC8: `docs/write.md` exists, names every new switch, gives sources and marks what is
    /// unverified. The defaults themselves are checked against the README by
    /// `drive_option_defaults_match_the_table_and_the_readme` in `self_tests.rs`.
    #[test]
    fn ac8_write_doc_names_each_switch_and_marks_unverified_behaviour() {
        let doc = include_str!("../docs/write.md");
        for switch in [
            "simple_replace_honours_if_match",
            "rename_sets_modified",
            "quota_bytes",
            "simple_upload_max_bytes",
            "rewrite",
            "rewrite_extensions",
            "folder_conflict_default",
            "replace_status",
        ] {
            assert!(
                doc.contains(&format!("`{switch}`")),
                "write.md lacks {switch}"
            );
        }
        assert!(doc.contains("**unverified**"));
        assert!(doc.contains("driveitem-put-content"));
        assert!(doc.contains("graph_api_and_windows_parity.md"));
        let readme = include_str!("../README.md");
        assert!(readme.contains("(docs/write.md)"));
    }

    /// `replace_status` decides the status of a successful replace, for the replace by ID
    /// and the create-by-name form under `replace`: `201` by default (the documented
    /// example), `200` with `Ok`. A new file is always `201`.
    #[tokio::test]
    async fn replace_status_follows_the_switch() {
        for (setting, expected) in [
            (ReplaceStatus::Created, StatusCode::CREATED),
            (ReplaceStatus::Ok, StatusCode::OK),
        ] {
            let options = DriveOptions {
                replace_status: setting,
                ..DriveOptions::default()
            };
            let (sim, drive, root) = sim_with(DriveType::Personal, options).await;
            let file = sim
                .create_file(&drive, &root, "f.txt", b"1".to_vec())
                .unwrap();
            let (status, item) = replace(&sim, &drive, &file, None, b"22").await;
            assert_eq!(status, expected, "{setting:?} by id");
            assert_eq!(item["size"], 2);
            let (status, item) =
                upload_new(&sim, &drive, &root, "F.TXT", Some("replace"), b"333").await;
            assert_eq!(status, expected, "{setting:?} by name");
            assert_eq!(item["id"], file.as_str());
            let (status, _) = upload_new(&sim, &drive, &root, "new.txt", None, b"4").await;
            assert_eq!(status, StatusCode::CREATED, "{setting:?} new file");
        }
    }

    /// An update naming `@microsoft.graph.conflictBehavior`, in the query string or the
    /// body, answers `400` and changes nothing: what Graph does with it is unknown and may
    /// be destructive, so the simulator refuses rather than answer a harmless `409`.
    #[tokio::test]
    async fn update_with_conflict_behaviour_is_400_and_changes_nothing() {
        let (sim, drive, root) = sim_with(DriveType::Personal, DriveOptions::default()).await;
        let file = sim.create_file(&drive, &root, "a", b"1".to_vec()).unwrap();
        sim.create_file(&drive, &root, "b", b"2".to_vec()).unwrap();
        let before = snapshot(&sim, &drive, &[&file]).await;
        let body = json!({"name": "b", "@microsoft.graph.conflictBehavior": "replace"});
        let (status, error) = patch(&sim, &drive, &file, None, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap()
                .contains("not simulated")
        );
        let url = format!(
            "{}?@microsoft.graph.conflictBehavior=rename",
            item_url(&sim, &drive, &file)
        );
        let body = serde_json::to_vec(&json!({"name": "c"})).unwrap();
        let (status, _) = send(&sim, Method::PATCH, &url, None, Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_unchanged(&sim, &drive, &[&file], &before).await;
    }

    /// An update with the conflict-behaviour name in another case, in the query string or
    /// the body, is refused like the documented spelling: a misspelt name must not get a
    /// destructive rename past the guard.
    #[tokio::test]
    async fn update_with_conflict_behaviour_in_another_case_is_400_and_changes_nothing() {
        let (sim, drive, root) = sim_with(DriveType::Personal, DriveOptions::default()).await;
        let file = sim.create_file(&drive, &root, "a", b"1".to_vec()).unwrap();
        sim.create_file(&drive, &root, "taken", b"2".to_vec())
            .unwrap();
        let before = snapshot(&sim, &drive, &[&file]).await;
        for key in [
            "@microsoft.graph.conflictbehavior",
            "@MICROSOFT.GRAPH.CONFLICTBEHAVIOR",
        ] {
            let url = format!("{}?{key}=replace", item_url(&sim, &drive, &file));
            let body = serde_json::to_vec(&json!({"name": "taken"})).unwrap();
            let (status, error) = send(&sim, Method::PATCH, &url, None, Some(body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{key} in the query");
            assert_eq!(error["error"]["code"], "invalidRequest");
            let mut body = serde_json::Map::new();
            body.insert("name".to_owned(), json!("taken"));
            body.insert(key.to_owned(), json!("replace"));
            let (status, _) = patch(&sim, &drive, &file, None, Value::Object(body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{key} in the body");
        }
        assert_unchanged(&sim, &drive, &[&file], &before).await;
    }

    /// The conflict behaviour is read only in its documented spelling, on every route that
    /// reads it: create by name (query string) and both create-folder routes (query string
    /// and body). A value in another case (`Fail`) or the name in another case answers
    /// `400` and changes nothing, so a client that misspells `fail` cannot pass a "never
    /// overwrite" test that real Graph might fail by falling back to `replace`.
    #[tokio::test]
    async fn conflict_behaviour_in_another_spelling_is_400_and_changes_nothing() {
        let (sim, drive, root) = sim_with(DriveType::Personal, DriveOptions::default()).await;
        let file = sim
            .create_file(&drive, &root, "report.docx", b"keep me".to_vec())
            .unwrap();
        sim.create_folder(&drive, &root, "Docs").unwrap();
        let before = snapshot(&sim, &drive, &[&file]).await;
        let key = "@microsoft.graph.conflictBehavior";
        let base = drive_url(&sim, &drive);
        let folder_routes = [
            format!("{base}/items/{root}/children"),
            format!("{base}/root/children"),
        ];
        let mut cases: Vec<(String, Method, String, Option<Vec<u8>>)> = Vec::new();
        for value in ["Fail", "FAIL", "Replace", "Rename", " fail"] {
            let url = format!("{base}/items/{root}:/report.docx:/content?{key}={value}");
            cases.push((
                format!("upload value {value:?}"),
                Method::PUT,
                url,
                Some(b"overwritten".to_vec()),
            ));
        }
        for spelt in [
            "@microsoft.graph.conflictbehavior",
            "@Microsoft.Graph.ConflictBehavior",
        ] {
            let url = format!("{base}/items/{root}:/report.docx:/content?{spelt}=fail");
            cases.push((
                format!("upload name {spelt}"),
                Method::PUT,
                url,
                Some(b"overwritten".to_vec()),
            ));
        }
        let both = format!(
            "{base}/items/{root}:/report.docx:/content?{key}=fail&@microsoft.graph.conflictbehavior=replace"
        );
        cases.push((
            "upload both spellings".to_owned(),
            Method::PUT,
            both,
            Some(b"overwritten".to_vec()),
        ));
        for route in &folder_routes {
            let folder = |fields: Value| Some(serde_json::to_vec(&fields).unwrap());
            for value in ["Fail", "RENAME"] {
                cases.push((
                    format!("{route} body value {value}"),
                    Method::POST,
                    route.clone(),
                    folder(json!({"name": "docs", "folder": {}, key: value})),
                ));
                cases.push((
                    format!("{route} query value {value}"),
                    Method::POST,
                    format!("{route}?{key}={value}"),
                    folder(json!({"name": "docs", "folder": {}})),
                ));
            }
            let spelt = "@microsoft.graph.conflictbehavior";
            cases.push((
                format!("{route} body name"),
                Method::POST,
                route.clone(),
                folder(json!({"name": "docs", "folder": {}, spelt: "fail"})),
            ));
            cases.push((
                format!("{route} query name"),
                Method::POST,
                format!("{route}?{spelt}=fail"),
                folder(json!({"name": "docs", "folder": {}})),
            ));
        }
        for (label, method, url, body) in cases {
            let (status, error) = send(&sim, method, &url, None, body).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{label}: {error}");
            assert_eq!(error["error"]["code"], "invalidRequest", "{label}");
        }
        assert_unchanged(&sim, &drive, &[&file], &before).await;
    }

    /// The quota is checked against the size stored at commit: in `Later` mode the
    /// uploaded size, so a 90-byte document fits a 100-byte quota and the rewrite that
    /// follows leaves the drive above it; in `InResponse` mode the rewritten size, so the
    /// same upload answers `507` and stores nothing.
    #[tokio::test]
    async fn quota_checks_the_size_stored_at_commit_in_each_rewrite_mode() {
        let document = vec![b'x'; 90];
        for mode in [RewriteMode::Later, RewriteMode::InResponse] {
            let options = DriveOptions {
                rewrite: mode,
                quota_bytes: 100,
                ..DriveOptions::default()
            };
            let (sim, drive, root) = sim_with(DriveType::DocumentLibrary, options).await;
            let (status, _) = upload_new(&sim, &drive, &root, "d.docx", None, &document).await;
            let used = get_json(&sim, &drive_url(&sim, &drive)).await["quota"]["used"]
                .as_u64()
                .unwrap();
            match mode {
                RewriteMode::Later => {
                    assert_eq!(status, StatusCode::CREATED);
                    assert!(used > 100, "the later rewrite leaves {used} bytes used");
                }
                _ => {
                    assert_eq!(status, StatusCode::INSUFFICIENT_STORAGE);
                    assert_eq!(used, 0);
                    let (feed, _) = enumerate(&sim, &drive).await;
                    assert_eq!(feed.len(), 1, "only the root: {feed:?}");
                }
            }
        }
    }

    /// The documented check order of a simple upload: the size limit comes before an
    /// unknown item for the replace by ID, and a bad conflict behaviour before the size
    /// limit for the create-by-name form.
    #[tokio::test]
    async fn simple_upload_refusal_order() {
        let options = DriveOptions {
            simple_upload_max_bytes: 4,
            ..DriveOptions::default()
        };
        let (sim, drive, root) = sim_with(DriveType::Personal, options).await;
        let missing = ItemId::new("NO-SUCH-ITEM").unwrap();
        let (status, _) = replace(&sim, &drive, &missing, None, b"too large").await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        let (status, _) = replace(&sim, &drive, &missing, None, b"ok").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = upload_new(&sim, &drive, &root, "n", Some("never"), b"too large").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = upload_new(&sim, &drive, &missing, "n", None, b"too large").await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    }
}

//! Writes made against the simulator over plain HTTP, read back through the
//! `mydrive-graph` client (WP-019, required test). The simulator may not depend on this
//! crate, so the test lives here.

// The helpers unwrap like the tests do; `cfg(test)` tells clippy they are test code
// (`allow-unwrap-in-tests` in `clippy.toml`).
#[cfg(test)]
mod sim_write_readback {
    use std::sync::Arc;

    use mydrive_core::hash::QuickXorHasher;
    use mydrive_core::ids::{DriveId, ItemId};
    use mydrive_graph::GraphClient;
    use mydrive_graph::auth::{AccessToken, BoxFuture, TokenError, TokenProvider};
    use mydrive_graph::client::{Download, UserAgent};
    use mydrive_graph::model::{DeltaContinuation, DeltaEntry, DriveItem, ItemKind, Timestamp};
    use mydrive_graph_sim::{DriveOptions, DriveType, GraphSim, RewriteMode, StatusCode};
    use serde_json::{Value, json};

    /// Always hands out the simulator's token.
    #[derive(Debug)]
    struct FixedToken(String);

    impl TokenProvider for FixedToken {
        fn access_token(&self) -> BoxFuture<'_, Result<AccessToken, TokenError>> {
            Box::pin(async move { Ok(AccessToken::new(self.0.clone())) })
        }

        fn force_refresh<'a>(
            &'a self,
            _rejected: &'a AccessToken,
        ) -> BoxFuture<'a, Result<AccessToken, TokenError>> {
            Box::pin(async move { Ok(AccessToken::new(self.0.clone())) })
        }
    }

    fn graph_client(sim: &GraphSim) -> GraphClient {
        let user_agent = UserAgent::new("Example", "my-drive-test", "0.0.1").unwrap();
        GraphClient::new(
            sim.base_url(),
            Arc::new(FixedToken(sim.token().to_owned())),
            user_agent,
        )
        .unwrap()
    }

    fn quick_xor(bytes: &[u8]) -> String {
        let mut hasher = QuickXorHasher::new();
        hasher.update(bytes);
        hasher.finalize().to_base64()
    }

    /// A plain HTTP write with the simulator's token; returns the status and JSON body.
    async fn write(
        sim: &GraphSim,
        method: reqwest::Method,
        path: &str,
        body: Vec<u8>,
    ) -> (StatusCode, Value) {
        let response = reqwest::Client::new()
            .request(method, format!("{}{path}", sim.base_url()))
            .bearer_auth(sim.token())
            .body(body)
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

    fn id_of(item: &Value) -> ItemId {
        ItemId::new(item["id"].as_str().unwrap()).unwrap()
    }

    async fn content(client: &GraphClient, drive: &DriveId, item: &DriveItem) -> Vec<u8> {
        let size = item.size.unwrap();
        match client.download(drive, &item.id, size, None).await.unwrap() {
            Download::WholeFile { body } => body.bytes().await.unwrap(),
            other => panic!("a download without a range is the whole file: {other:?}"),
        }
    }

    async fn delta_items(client: &GraphClient, drive: &DriveId) -> Vec<DriveItem> {
        let mut page = client.delta_from_root(drive).await.unwrap();
        let mut items = Vec::new();
        for _ in 0..1000 {
            for entry in std::mem::take(&mut page.entries) {
                match entry {
                    DeltaEntry::Item(item) => items.push(*item),
                    DeltaEntry::Problem(problem) => panic!("unusable entry: {problem:?}"),
                }
            }
            match page.continuation {
                DeltaContinuation::DeltaLink(_) => return items,
                DeltaContinuation::NextLink(next) => {
                    page = client.delta_from_link(drive, &next).await.unwrap();
                }
            }
        }
        panic!("delta did not end");
    }

    /// A folder created, a file uploaded into it, renamed and given a `fileSystemInfo`
    /// time over HTTP read back through the client: same IDs, names, parent, eTag, size,
    /// quickXorHash, whole-second time and content, in get-item and in delta.
    #[tokio::test]
    async fn writes_read_back_through_the_graph_client() {
        let sim = GraphSim::start().await.unwrap();
        let drive = sim.add_drive(DriveType::Business).unwrap();
        let root = sim.root_id(&drive).unwrap();
        let client = graph_client(&sim);

        let folder =
            json!({"name": "Docs", "folder": {}, "@microsoft.graph.conflictBehavior": "fail"});
        let (status, folder) = write(
            &sim,
            reqwest::Method::POST,
            &format!("/drives/{drive}/items/{root}/children"),
            serde_json::to_vec(&folder).unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let folder = id_of(&folder);

        let bytes = b"written through the simulator's write side".to_vec();
        let (status, uploaded) = write(
            &sim,
            reqwest::Method::PUT,
            &format!("/drives/{drive}/items/{folder}:/a.txt:/content?@microsoft.graph.conflictBehavior=fail"),
            bytes.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let file = id_of(&uploaded);

        let update = json!({"name": "b.txt", "fileSystemInfo": {"lastModifiedDateTime": "2025-06-07T08:09:10.5Z"}});
        let (status, renamed) = write(
            &sim,
            reqwest::Method::PATCH,
            &format!("/drives/{drive}/items/{file}"),
            serde_json::to_vec(&update).unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let item = client.get_item(&drive, &file).await.unwrap();
        assert_eq!(item.name.as_deref(), Some("b.txt"));
        assert_eq!(item.etag.as_deref(), renamed["eTag"].as_str());
        assert_eq!(item.size, Some(bytes.len() as u64));
        assert_eq!(
            item.quick_xor_hash().map(|hash| hash.to_base64()),
            Some(quick_xor(&bytes))
        );
        assert_eq!(
            item.parent.as_ref().and_then(|p| p.id.as_ref()),
            Some(&folder)
        );
        let expected = Timestamp::parse_rfc3339("2025-06-07T08:09:10Z").unwrap();
        assert_eq!(
            item.file_system_info
                .as_ref()
                .and_then(|info| info.last_modified),
            Some(expected)
        );
        assert_eq!(content(&client, &drive, &item).await, bytes);

        let in_delta = delta_items(&client, &drive).await;
        let seen = in_delta.iter().find(|i| i.id == file).unwrap();
        assert_eq!(seen.etag, item.etag);
        assert_eq!(seen.size, item.size);
        assert!(matches!(
            in_delta.iter().find(|i| i.id == folder).unwrap().kind,
            ItemKind::Folder(_)
        ));

        let (status, _) = write(
            &sim,
            reqwest::Method::DELETE,
            &format!("/drives/{drive}/items/{folder}"),
            Vec::new(),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(client.get_item(&drive, &file).await.is_err());
    }

    /// A library's later rewrite, read through the client: the reported size and hash
    /// are the rewritten bytes', the download passes the client's size check and hashes
    /// to the reported hash, and neither equals the upload. Supports clients implementing
    /// ADR-0012 R8.
    #[tokio::test]
    async fn library_rewrite_reads_back_consistently_through_the_graph_client() {
        let sim = GraphSim::start().await.unwrap();
        let options = DriveOptions {
            rewrite: RewriteMode::Later,
            ..DriveOptions::default()
        };
        let drive = sim
            .add_drive_with(DriveType::DocumentLibrary, options)
            .unwrap();
        let root = sim.root_id(&drive).unwrap();
        let client = graph_client(&sim);
        let bytes = b"an office document".to_vec();
        let (status, uploaded) = write(
            &sim,
            reqwest::Method::PUT,
            &format!("/drives/{drive}/items/{root}:/plan.xlsx:/content"),
            bytes.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(
            uploaded["file"]["hashes"]["quickXorHash"],
            quick_xor(&bytes)
        );

        let item = client.get_item(&drive, &id_of(&uploaded)).await.unwrap();
        let served = content(&client, &drive, &item).await;
        assert_ne!(served, bytes);
        assert_eq!(item.size, Some(served.len() as u64));
        assert_eq!(
            item.quick_xor_hash().map(|hash| hash.to_base64()),
            Some(quick_xor(&served))
        );
        assert_ne!(item.etag.as_deref(), uploaded["eTag"].as_str());
    }
}

//! Delta, drive and item reads against the simulator: AC1, AC9, AC10 (repeats, folder
//! deletes, hashes, `parentReference.path`), ADR-0011 R2, R4, R8, R13.

#[path = "client_support.rs"]
mod support;

use std::collections::HashSet;

use mydrive_core::hash::QuickXorHasher;
use mydrive_graph::GraphErrorKind;
use mydrive_graph::model::{DeltaLink, DriveType as ModelDriveType, ItemKind};
use mydrive_graph_sim::{DeltaRepeats, DriveOptions, DriveType, FolderDeletes, Origin};
use support::{TestResult, client, full_delta, ids, sim_with, test_user_agent};

fn small_pages() -> DriveOptions {
    DriveOptions {
        delta_page_size: 2,
        ..DriveOptions::default()
    }
}

/// AC1: paging a multi-page delta yields every item and a delta link; resuming from the
/// link yields only the changes made after it.
#[tokio::test]
async fn ac1_multi_page_delta_then_only_later_changes() -> TestResult {
    let (sim, drive, root) = sim_with(DriveType::Business, small_pages()).await;
    let docs = sim.create_folder(&drive, &root, "Docs")?;
    let mut expected: HashSet<String> = [root.to_string(), docs.to_string()].into();
    for n in 0..5 {
        let file = sim.create_file(&drive, &docs, &format!("f{n}.txt"), vec![n; 10])?;
        expected.insert(file.to_string());
    }
    let client = client(&sim);

    let (items, link, pages) = full_delta(&client, &drive, None).await;
    assert!(pages > 1, "the feed must span several pages, got {pages}");
    assert_eq!(ids(&items).into_iter().collect::<HashSet<_>>(), expected);
    assert_eq!(
        items.len(),
        expected.len(),
        "a token-less round lists each item once"
    );

    // Nothing changed: the link yields no items.
    let (none, link, _) = full_delta(&client, &drive, Some(&link)).await;
    assert!(none.is_empty(), "{none:?}");

    let changed = sim.create_file(&drive, &root, "later.txt", b"new".to_vec())?;
    let first = sim.create_file(&drive, &docs, "z.txt", b"z".to_vec())?;
    sim.modify_file(&drive, &first, b"zz".to_vec())?;
    let (later, _, _) = full_delta(&client, &drive, Some(&link)).await;
    let later_ids: HashSet<String> = ids(&later).into_iter().collect();
    assert_eq!(later_ids, [changed.to_string(), first.to_string()].into());
    Ok(())
}

/// AC10 (both repeat modes), ADR-0011 R2 (client side): the client keeps every occurrence
/// in feed order, so the engine can apply the last one; the last carries the latest
/// state.
#[tokio::test]
async fn adr_0011_r2_repeats_are_kept_in_feed_order() -> TestResult {
    for (mode, occurrences) in [
        (DeltaRepeats::EveryChange, 3),
        (DeltaRepeats::LatestStateOnce, 1),
    ] {
        let options = DriveOptions {
            delta_repeats: mode,
            ..small_pages()
        };
        let (sim, drive, root) = sim_with(DriveType::Personal, options).await;
        let file = sim.create_file(&drive, &root, "a.txt", b"1".to_vec())?;
        let client = client(&sim);
        let (_, link, _) = full_delta(&client, &drive, None).await;

        sim.modify_file(&drive, &file, b"22".to_vec())?;
        sim.create_file(&drive, &root, "b.txt", b"b".to_vec())?;
        sim.modify_file(&drive, &file, b"333".to_vec())?;
        sim.modify_file(&drive, &file, b"4444".to_vec())?;
        let (items, _, _) = full_delta(&client, &drive, Some(&link)).await;
        let of_file: Vec<_> = items.iter().filter(|i| i.id == file).collect();
        assert_eq!(of_file.len(), occurrences, "{mode:?}");
        let last = of_file.last().unwrap();
        assert_eq!(
            last.size,
            Some(4),
            "{mode:?}: the last occurrence is the latest"
        );
    }
    Ok(())
}

/// AC10 (both folder-delete modes): deleted items carry the `deleted` facet; the client
/// reports exactly what the feed says.
#[tokio::test]
async fn folder_deletes_in_both_modes() -> TestResult {
    for (mode, drive_type) in [
        (FolderDeletes::FolderOnly, DriveType::Business),
        (FolderDeletes::EveryDescendant, DriveType::Personal),
    ] {
        let options = DriveOptions {
            folder_deletes: mode,
            ..DriveOptions::default()
        };
        let (sim, drive, root) = sim_with(drive_type, options).await;
        let folder = sim.create_folder(&drive, &root, "Old")?;
        let child = sim.create_file(&drive, &folder, "c.txt", b"c".to_vec())?;
        let client = client(&sim);
        let (_, link, _) = full_delta(&client, &drive, None).await;

        sim.delete(&drive, &folder)?;
        let (items, _, _) = full_delta(&client, &drive, Some(&link)).await;
        assert!(items.iter().all(|i| i.deleted), "{mode:?}: {items:?}");
        let expected = match mode {
            FolderDeletes::FolderOnly => vec![folder.to_string()],
            FolderDeletes::EveryDescendant => vec![folder.to_string(), child.to_string()],
        };
        assert_eq!(ids(&items), expected, "{mode:?}");
        let deleted_folder = &items[0];
        if drive_type == DriveType::Business {
            assert!(
                deleted_folder.name.is_none(),
                "business deletes omit the name"
            );
        } else {
            assert!(
                deleted_folder.size.is_none(),
                "personal deletes omit the size"
            );
        }
    }
    Ok(())
}

/// AC9, ADR-0011 R4: the shapes delta really sends parse: the root item, no cTag on
/// business delta, drive IDs normalised, item IDs exact.
#[tokio::test]
async fn ac9_delta_item_shapes() -> TestResult {
    for drive_type in [
        DriveType::Personal,
        DriveType::Business,
        DriveType::DocumentLibrary,
    ] {
        let (sim, drive, root) = sim_with(drive_type, DriveOptions::default()).await;
        let folder = sim.create_folder(&drive, &root, "F")?;
        let file = sim.create_file(&drive, &folder, "x.bin", vec![1, 2, 3])?;
        let client = client(&sim);
        let (items, _, _) = full_delta(&client, &drive, None).await;

        let root_item = items.iter().find(|i| i.id == root).unwrap();
        assert!(root_item.root && root_item.is_folder());
        assert!(root_item.parent.as_ref().unwrap().id.is_none());
        let file_item = items.iter().find(|i| i.id == file).unwrap();
        assert_eq!(file_item.id.as_str(), file.as_str(), "item IDs are exact");
        let parent = file_item.parent.as_ref().unwrap();
        assert_eq!(parent.id.as_ref(), Some(&folder));
        assert_eq!(parent.drive_id.as_ref(), Some(&drive));
        assert_eq!(
            parent.path, None,
            "delta never carries parentReference.path"
        );
        assert_eq!(file_item.size, Some(3));
        assert!(file_item.etag.is_some() && file_item.last_modified.is_some());
        let expected_type = match drive_type {
            DriveType::Personal => ModelDriveType::Personal,
            DriveType::Business => ModelDriveType::Business,
            DriveType::DocumentLibrary => ModelDriveType::DocumentLibrary,
        };
        assert_eq!(parent.drive_type.as_ref(), Some(&expected_type));
        if drive_type == DriveType::Personal {
            assert!(file_item.ctag.is_some());
        } else {
            assert!(file_item.ctag.is_none(), "business delta omits cTag");
        }
        // Folders never carry a cTag by default.
        assert!(
            items
                .iter()
                .filter(|i| i.is_folder())
                .all(|i| i.ctag.is_none())
        );
    }
    Ok(())
}

/// AC10 (hashes present and absent), ADR-0011 R4: quickXorHash is decoded and matches the
/// content; a zero-byte file without a hash reads as "no hash", not as some value.
#[tokio::test]
async fn adr_0011_r4_quick_xor_hash_present_and_absent() -> TestResult {
    for hash_empty_files in [false, true] {
        let options = DriveOptions {
            hash_empty_files,
            ..DriveOptions::default()
        };
        let (sim, drive, root) = sim_with(DriveType::Business, options).await;
        let content = b"some content".to_vec();
        let full = sim.create_file(&drive, &root, "full", content.clone())?;
        let empty = sim.create_file(&drive, &root, "empty", Vec::new())?;
        let client = client(&sim);
        let (items, _, _) = full_delta(&client, &drive, None).await;

        let mut hasher = QuickXorHasher::new();
        hasher.update(&content);
        let full_item = items.iter().find(|i| i.id == full).unwrap();
        assert_eq!(full_item.quick_xor_hash(), Some(&hasher.finalize()));

        let empty_item = items.iter().find(|i| i.id == empty).unwrap();
        assert!(empty_item.is_file());
        assert_eq!(
            empty_item.quick_xor_hash(),
            hash_empty_files
                .then(|| QuickXorHasher::new().finalize())
                .as_ref()
        );
        let by_get = client.get_item(&drive, &empty).await?;
        assert_eq!(by_get.quick_xor_hash().is_some(), hash_empty_files);
    }
    Ok(())
}

/// AC10 (`parentReference.path` present and absent): get-item carries it unless the
/// switch removes it; the item still parses either way.
#[tokio::test]
async fn parent_path_present_and_absent() -> TestResult {
    for with_path in [true, false] {
        let options = DriveOptions {
            parent_path_outside_delta: with_path,
            ..DriveOptions::default()
        };
        let (sim, drive, root) = sim_with(DriveType::Personal, options).await;
        let folder = sim.create_folder(&drive, &root, "My Docs")?;
        let file = sim.create_file(&drive, &folder, "n.txt", b"n".to_vec())?;
        let client = client(&sim);
        let item = client.get_item(&drive, &file).await?;
        let path = item.parent.unwrap().path;
        if with_path {
            assert_eq!(path.as_deref(), Some("/drive/root:/My%20Docs"));
        } else {
            assert_eq!(path, None);
        }
        let root_item = client.get_root(&drive).await?;
        assert!(root_item.root);
        assert!(root_item.parent.unwrap().path.is_none());
    }
    Ok(())
}

/// The drive resource parses, with its ID normalised and its quota.
#[tokio::test]
async fn get_drive_parses() -> TestResult {
    let (sim, drive, root) = sim_with(DriveType::DocumentLibrary, DriveOptions::default()).await;
    sim.create_file(&drive, &root, "a", vec![0; 100])?;
    let parsed = client(&sim).get_drive(&drive).await?;
    assert_eq!(parsed.id, drive);
    assert_eq!(parsed.drive_type, Some(ModelDriveType::DocumentLibrary));
    assert_eq!(parsed.quota.unwrap().used, Some(100));
    Ok(())
}

/// A file item from get-item has the file facet; the pre-authenticated
/// `@microsoft.graph.downloadUrl` that get-item carries in redirect mode is not kept.
#[tokio::test]
async fn get_item_file_facet_without_download_url() -> TestResult {
    let (sim, drive, root) = sim_with(DriveType::Personal, DriveOptions::default()).await;
    let file = sim.create_file(&drive, &root, "a", vec![0; 3])?;
    let item = client(&sim).get_item(&drive, &file).await?;
    assert!(matches!(item.kind, ItemKind::File(_)));
    let printed = format!("{item:?}");
    assert!(!printed.contains("tempauth") && !printed.contains(sim.download_origin()));
    Ok(())
}

/// A link that is not under the client's Graph base URL is refused without a request, so
/// the bearer token cannot be sent to it.
#[tokio::test]
async fn foreign_delta_link_is_refused_without_a_request() -> TestResult {
    let (sim, drive, _) = sim_with(DriveType::Personal, DriveOptions::default()).await;
    let client = client(&sim);
    let graph = sim.base_url().trim_end_matches("/v1.0").to_owned();
    for link in [
        format!("{}/v1.0/drives/{drive}/root/delta", sim.download_origin()),
        format!("{graph}/v2.0/drives/{drive}/root/delta"),
        format!("{graph}/v1.0x/drives/{drive}/root/delta"),
        "not a url".to_owned(),
    ] {
        let err = client
            .delta_from_link(&drive, &DeltaLink::new(link.clone()))
            .await
            .unwrap_err();
        assert!(
            matches!(err.kind(), GraphErrorKind::ForeignLink),
            "{link}: {err}"
        );
    }
    assert!(sim.requests().is_empty());
    Ok(())
}

/// ADR-0011 R8: every request, on both origins, carries the decorated User-Agent.
#[tokio::test]
async fn adr_0011_r8_every_request_carries_user_agent() -> TestResult {
    let (sim, drive, root) = sim_with(DriveType::Business, small_pages()).await;
    for n in 0..3 {
        sim.create_file(&drive, &root, &format!("{n}"), vec![n; 4])?;
    }
    let file = sim.create_file(&drive, &root, "dl", vec![9; 8])?;
    let client = client(&sim);
    client.get_drive(&drive).await?;
    full_delta(&client, &drive, None).await;
    client.download(&drive, &file, None).await?.bytes().await?;
    let _ = client.get_item(&drive, &"missing".parse()?).await;

    let log = sim.requests();
    assert!(log.iter().any(|r| r.origin == Origin::Download));
    assert!(log.len() > 4);
    let expected = test_user_agent();
    for request in &log {
        assert_eq!(
            request.header("user-agent"),
            Some(expected.as_str()),
            "{request:?}"
        );
    }
    assert!(expected.as_str().starts_with("ISV|"));
    Ok(())
}

/// ADR-0011 R13: no code in this crate calls `/me/drive/sharedWithMe`.
#[test]
fn adr_0011_r13_no_shared_with_me() -> TestResult {
    let mut stack = vec![std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")];
    let mut checked = 0;
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path)?;
                assert!(
                    !text.to_ascii_lowercase().contains("sharedwithme"),
                    "{}",
                    path.display()
                );
                checked += 1;
            }
        }
    }
    assert!(checked > 5);
    Ok(())
}

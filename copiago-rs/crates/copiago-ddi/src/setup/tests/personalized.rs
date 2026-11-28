use super::*;

pub(super) const IMAGE: &[u8] = b"personalized simulation";

#[tokio::test]
async fn personalized_reuses_cached_ticket_or_prepares_tss_after_cache_miss() {
    let root = TempRoot::new();
    fs::write(root.0.join("Image.dmg"), IMAGE).unwrap();
    fs::write(root.0.join("Image.trustcache"), b"trust").unwrap();
    let identity = fields([
        ("ApBoardID", "0x01".into()),
        ("ApChipID", "0x02".into()),
        ("Manifest", Value::Dictionary(Dictionary::new())),
    ]);
    Value::Dictionary(fields([(
        "BuildIdentities",
        Value::Array(vec![Value::Dictionary(identity)]),
    )]))
    .to_file_xml(root.0.join("BuildManifest.plist"))
    .unwrap();
    let ticket = b"synthetic ticket".to_vec();
    let missing_manifest = fields([
        ("Error", "InternalError".into()),
        (
            "DetailedError",
            concat!(
                "Failed to copy personalization manifest. Failed to find a cached manifest ",
                "(im4m) for variant DeveloperDiskImage (personalization required)."
            )
            .into(),
        ),
    ]);
    for (version, cached, manifest_reply) in [
        (
            "26.4",
            true,
            fields([("ImageSignature", Value::Data(ticket.clone()))]),
        ),
        ("26.4", false, Dictionary::new()),
        ("18.5", false, missing_manifest),
        // A future iOS may word the cache miss differently; it must still reach signing.
        (
            "18.6",
            false,
            fields([
                ("Error", "InternalError".into()),
                ("DetailedError", "Unrecognized manifest failure".into()),
            ]),
        ),
    ] {
        let mut steps = mounter_steps(true);
        let mut rsd = handshake(version);
        rsd.properties
            .insert("UniqueChipID".into(), Value::from(3u64));
        let (upload, complete) = (
            (
                "ReceiveBytes",
                fields([("Status", "ReceiveBytesAck".into())]),
            ),
            ("MountImage", fields([("Status", "Complete".into())])),
        );
        steps.push(("QueryPersonalizationManifest", manifest_reply));
        if cached {
            steps.extend([upload.clone(), complete.clone()]);
        }
        let (mut provider, initial) = mounter(steps);
        let mut reopened = None;
        if !cached {
            let identifiers = fields([
                ("BoardId", 1u64.into()),
                ("ChipID", 2u64.into()),
                ("Ap,PersonalizationTag", "synthetic".into()),
            ]);
            let (mut extra, script) = mounter(vec![
                (
                    "QueryPersonalizationIdentifiers",
                    fields([("PersonalizationIdentifiers", Value::Dictionary(identifiers))]),
                ),
                (
                    "QueryNonce",
                    fields([("PersonalizationNonce", Value::Data(vec![0x10]))]),
                ),
                upload,
                complete,
            ]);
            provider.streams.append(&mut extra.streams);
            reopened = Some(script);
        }
        let mut updates = Vec::new();
        let mut log = SetupLog::new(&root.0);
        let result = install_image(
            &mut provider,
            &mut rsd,
            ImageKind::Personalized,
            &root.0,
            &mut log,
            |update| {
                updates.push(update);
                ControlFlow::Continue(())
            },
            async |request, kind| {
                assert!(!cached);
                assert_eq!(kind, ImageKind::Personalized);
                assert_eq!(request["@ApImg4Ticket"].as_boolean(), Some(true));
                assert_eq!(request["ApECID"].as_unsigned_integer(), Some(3));
                assert_eq!(request["ApNonce"].as_data(), Some([0x10].as_slice()));
                assert_eq!(
                    request["Ap,PersonalizationTag"].as_string(),
                    Some("synthetic")
                );
                Ok(ticket.clone())
            },
        );
        timeout(Duration::from_secs(5), result)
            .await
            .unwrap()
            .unwrap();
        if cached {
            assert_eq!(updates, [SetupUpdate::Preparing, SetupUpdate::Installing]);
        } else {
            assert_eq!(
                updates,
                [
                    SetupUpdate::Preparing,
                    SetupUpdate::Signing,
                    SetupUpdate::Installing
                ]
            );
        }
        assert!(provider.streams.is_empty());
        initial.await.unwrap();
        if let Some(reopened) = reopened {
            reopened.await.unwrap();
        }
    }
}

#[tokio::test]
async fn actionable_manifest_errors_stop_and_log_the_failed_command() {
    for (reply, expected, cause) in [(
        fields([("Error", "DeviceLocked".into())]),
        "PasswordRequiredError",
        DdiCause::DeviceLocked,
    )] {
        let root = TempRoot::new();
        fs::write(root.0.join("Image.dmg"), IMAGE).unwrap();
        fs::write(root.0.join("Image.trustcache"), b"trust").unwrap();
        let mut steps = mounter_steps(true);
        steps.push(("QueryPersonalizationManifest", reply));
        let (mut provider, script) = mounter(steps);
        let mut rsd = handshake("18.5");
        let result = install_image(
            &mut provider,
            &mut rsd,
            ImageKind::Personalized,
            &root.0,
            &mut SetupLog::new(&root.0),
            |update| {
                assert_eq!(update, SetupUpdate::Preparing);
                ControlFlow::Continue(())
            },
            async |_, _| panic!("a device error must stop before signing"),
        )
        .await;
        assert_eq!(
            result,
            Err(SetupError::Failed {
                stage: DdiStage::Install,
                timed_out: false,
                cause,
            })
        );
        let saved = fs::read_to_string(root.0.join("developer-setup.log")).unwrap();
        assert!(saved.contains(&format!(
            "error={expected} step=QueryPersonalizationManifest"
        )));
        script.await.unwrap();
    }
}

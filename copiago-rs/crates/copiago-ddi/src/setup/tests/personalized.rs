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
    for cached in [true, false] {
        let mut steps = mounter_steps(true);
        let mut rsd = handshake("26.4");
        rsd.properties
            .insert("UniqueChipID".into(), Value::from(3u64));
        let (upload, complete) = (
            (
                "ReceiveBytes",
                fields([("Status", "ReceiveBytesAck".into())]),
            ),
            ("MountImage", fields([("Status", "Complete".into())])),
        );
        let manifest_reply = if cached {
            fields([("ImageSignature", Value::Data(ticket.clone()))])
        } else {
            Dictionary::new()
        };
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
        within(Duration::from_secs(5), async {
            let mut image =
                mount::prepare_image(&mut provider, &mut rsd, ImageKind::Personalized, &root.0)
                    .await?
                    .unwrap();
            if cached {
                assert_eq!(image.take_cached_ticket(), Some(ticket.clone()));
            } else {
                assert_eq!(image.take_cached_ticket(), None);
                let request = image.signature_request(&mut provider, &mut rsd).await?;
                assert_eq!(request["@ApImg4Ticket"].as_boolean(), Some(true));
                assert_eq!(request["ApECID"].as_unsigned_integer(), Some(3));
                assert_eq!(request["ApNonce"].as_data(), Some([0x10].as_slice()));
                assert_eq!(
                    request["Ap,PersonalizationTag"].as_string(),
                    Some("synthetic")
                );
            }
            image.install(&mut provider, &mut rsd, ticket.clone()).await
        })
        .await
        .unwrap();
        assert!(provider.streams.is_empty());
        initial.await.unwrap();
        if let Some(reopened) = reopened {
            reopened.await.unwrap();
        }
    }
}

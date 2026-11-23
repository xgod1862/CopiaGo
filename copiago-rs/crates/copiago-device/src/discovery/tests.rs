use std::collections::VecDeque;
use std::net::{SocketAddrV4, SocketAddrV6};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use idevice::remote_pairing::compute_auth_tag;

use super::*;

const TEST_IRK: [u8; 16] = [33; 16];

fn advertisement(ipv4: bool, tagged: bool) -> Advertisement {
    let identifier = "test-service-id";
    Advertisement {
        ipv4: ipv4.then(|| "192.0.2.7:49152".parse::<SocketAddrV4>().unwrap()),
        ipv6: Some("[fe80::123%7]:49152".parse::<SocketAddrV6>().unwrap()),
        identifier: Some(identifier.to_string()),
        auth_tag: tagged.then(|| STANDARD.encode(compute_auth_tag(&TEST_IRK, identifier))),
    }
}

#[test]
fn validates_tag_against_saved_irk_and_advertised_identifier() {
    let matching = advertisement(true, true)
        .into_candidate(Some(&TEST_IRK))
        .unwrap();
    assert!(matching.auth_tag_matches());
    assert_eq!(
        matching
            .addresses()
            .map(|ip| ip.is_ipv4())
            .collect::<Vec<_>>(),
        [true, false]
    );

    assert!(
        advertisement(true, true)
            .into_candidate(Some(&[34; 16]))
            .is_none()
    );
    let mut invalid = advertisement(true, true);
    invalid.identifier = Some("different-id".to_string());
    assert!(invalid.into_candidate(Some(&TEST_IRK)).is_none());

    let mut invalid = advertisement(true, true);
    invalid.auth_tag = Some(String::new());
    assert!(invalid.into_candidate(Some(&TEST_IRK)).is_none());

    let mut invalid = advertisement(true, true);
    invalid.identifier = None;
    assert!(invalid.into_candidate(Some(&TEST_IRK)).is_none());
}

#[test]
fn automatic_connection_excludes_advertisements_without_matching_material() {
    let tagless = advertisement(true, false)
        .into_candidate(Some(&TEST_IRK))
        .unwrap();
    let old_record = advertisement(true, true).into_candidate(None).unwrap();
    let matched = advertisement(true, true)
        .into_candidate(Some(&TEST_IRK))
        .unwrap();
    assert!(matched_addresses(&[tagless, old_record]).is_empty());
    assert_eq!(
        matched_addresses(&[tagless, matched, old_record]),
        matched.addresses().collect::<Vec<_>>()
    );
}

#[tokio::test(start_paused = true)]
async fn returns_tag_match_over_ipv4_first_and_keeps_alternates() {
    let start = Instant::now();
    let mut answers = VecDeque::from([
        advertisement(true, false),
        advertisement(false, true),
        advertisement(true, true),
    ]);
    let found = collect_candidates(start + DISCOVERY_TIMEOUT, Some(&TEST_IRK), async || {
        tokio::time::sleep(Duration::from_millis(100)).await;
        answers.pop_front()
    })
    .await;

    assert_eq!(start.elapsed(), Duration::from_millis(300));
    assert_eq!(found.len(), 3);
    assert!(found[0].auth_tag_matches() && found[0].addresses().next().unwrap().is_ipv4());
    assert!(found[1].auth_tag_matches() && found[1].addresses().next().unwrap().is_ipv6());
    assert!(!found[2].auth_tag_matches());
}

#[tokio::test(start_paused = true)]
async fn stops_at_five_seconds_without_a_reply() {
    let start = Instant::now();
    let found = collect_candidates(start + DISCOVERY_TIMEOUT, Some(&TEST_IRK), async || {
        std::future::pending().await
    })
    .await;
    assert!(found.is_empty());
    assert_eq!(start.elapsed(), DISCOVERY_TIMEOUT);
}

#[tokio::test(start_paused = true)]
async fn returns_ipv6_match_at_deadline_when_ipv4_is_unavailable() {
    let start = Instant::now();
    let mut first = Some(advertisement(false, true));
    let found = collect_candidates(
        start + DISCOVERY_TIMEOUT,
        Some(&TEST_IRK),
        async || match first.take() {
            Some(answer) => Some(answer),
            None => std::future::pending().await,
        },
    )
    .await;

    assert_eq!(start.elapsed(), DISCOVERY_TIMEOUT);
    assert_eq!(found.len(), 1);
    assert!(found[0].auth_tag_matches());
    assert!(found[0].addresses().next().unwrap().is_ipv6());
}

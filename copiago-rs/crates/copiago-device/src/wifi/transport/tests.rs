use std::net::Ipv6Addr;

use idevice::tcp::adapter::Adapter;
use tokio::io::{AsyncReadExt, duplex};

use super::*;

#[tokio::test]
async fn close_releases_both_streams_during_a_pending_adapter_connect() {
    let (data, mut data_peer) = duplex(1024);
    let (control, mut control_peer) = duplex(1024);
    let (stream, lifetime) = Transport::new(data, control);
    let mut adapter = Adapter::new(
        Box::new(stream),
        Ipv6Addr::LOCALHOST.into(),
        Ipv6Addr::LOCALHOST.into(),
    )
    .to_async_handle();

    let pending = tokio::spawn(async move { adapter.connect(5000).await });
    let mut syn = [0; 256];
    assert_ne!(
        timeout(Duration::from_secs(1), data_peer.read(&mut syn))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    let mut byte = [0];
    assert!(
        timeout(Duration::ZERO, control_peer.read(&mut byte))
            .await
            .is_err()
    );

    lifetime.close().await.unwrap();
    assert!(pending.await.unwrap().is_err());
    assert_eq!(data_peer.read(&mut byte).await.unwrap(), 0);
    assert_eq!(control_peer.read(&mut byte).await.unwrap(), 0);
}

#[tokio::test]
async fn loss_of_data_transport_releases_control_while_handle_is_alive() {
    let (data, data_peer) = duplex(1024);
    let (control, mut control_peer) = duplex(1024);
    let (stream, lifetime) = Transport::new(data, control);
    let mut adapter = Adapter::new(
        Box::new(stream),
        Ipv6Addr::LOCALHOST.into(),
        Ipv6Addr::LOCALHOST.into(),
    )
    .to_async_handle();
    drop(data_peer);

    assert_eq!(
        timeout(Duration::from_secs(1), control_peer.read(&mut [0]))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    assert!(adapter.connect(5000).await.is_err());
    lifetime.close().await.unwrap();
}

use copiago_core::i18n::Language;
use idevice::Idevice;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream, duplex};

use super::*;

#[tokio::test]
async fn developer_menu_only_requests_visibility_and_checks_the_device_reply() {
    for (response, accepted) in [
        (Dictionary::from_iter([("success", true)]), true),
        (Dictionary::from_iter([("success", false)]), false),
        (Dictionary::from_iter([("Error", "DeviceLocked")]), false),
        (Dictionary::new(), false),
    ] {
        let (host, mut phone) = duplex(4096);
        let fixture = async move {
            let request = receive(&mut phone).await;
            assert_eq!(request, Dictionary::from_iter([("action", 0_u64)]));
            reply(&mut phone, response).await;
            assert_eq!(phone.read(&mut [0; 1]).await.unwrap(), 0);
        };
        let (result, ()) = tokio::join!(crate::usb::request_developer_menu(host), fixture);
        assert_eq!(result.is_ok(), accepted);
    }
}

#[tokio::test]
async fn wifi_connections_are_enabled_when_false_or_unset() {
    for reported in [Some(true), Some(false), None] {
        let (host, mut phone) = duplex(4096);
        let mut lockdown = LockdownClient::new(Idevice::new(Box::new(host), "test"));
        let fixture = async move {
            let request = receive(&mut phone).await;
            assert_request(&request, "Request", "GetValue");
            assert_request(&request, "Key", "EnableWifiConnections");
            assert_request(&request, "Domain", "com.apple.mobile.wireless_lockdown");
            let response = match reported {
                Some(value) => Dictionary::from_iter([("Value", value)]),
                None => Dictionary::from_iter([("Error", "MissingValue")]),
            };
            reply(&mut phone, response).await;
            if reported != Some(true) {
                let request = receive(&mut phone).await;
                assert_request(&request, "Request", "SetValue");
                assert_request(&request, "Key", "EnableWifiConnections");
                assert_request(&request, "Domain", "com.apple.mobile.wireless_lockdown");
                assert_eq!(request.get("Value"), Some(&true.into()));
                reply(&mut phone, Dictionary::new()).await;
            }
        };
        let (result, ()) = tokio::join!(enable_wifi_connections(&mut lockdown), fixture);
        assert!(result.is_ok());
    }
}

#[tokio::test]
async fn lockdown_refusal_uses_the_python_trust_message() {
    let (host, mut phone) = duplex(16_384);
    let mut lockdown = LockdownClient::new(Idevice::new(Box::new(host), "test"));
    let fixture = async {
        let request = receive(&mut phone).await;
        assert_request(&request, "Key", "DevicePublicKey");
        reply(
            &mut phone,
            Dictionary::from_iter([("Value", Value::Data(DEVICE_PUBLIC_KEY.as_bytes().to_vec()))]),
        )
        .await;
        let request = receive(&mut phone).await;
        assert_request(&request, "Key", "WiFiAddress");
        reply(
            &mut phone,
            Dictionary::from_iter([("Value", "00:00:00:00:00:00")]),
        )
        .await;
        let request = receive(&mut phone).await;
        assert_request(&request, "Request", "Pair");
        reply(
            &mut phone,
            Dictionary::from_iter([("Error", "UserDeniedPairing")]),
        )
        .await;
    };
    let (result, ()) = tokio::join!(lockdown.pair("fixture-host", "fixture-buid", None), fixture);
    let error = PairingError::from(result.unwrap_err());
    assert_eq!(error, PairingError::Denied);
    assert_eq!(
        error.message().render(Language::ZhCn),
        "iPhone 上选择了「不信任」。请重新插拔数据线，在提示时点「信任」，再点「连接 iPhone」"
    );
    assert_eq!(
        error.message().render(Language::En),
        "Trust was declined on the iPhone. Reconnect the USB cable, tap Trust when asked, then select Connect iPhone."
    );
}

fn assert_request(request: &Dictionary, key: &str, expected: &str) {
    assert_eq!(request.get(key).and_then(Value::as_string), Some(expected));
}

async fn receive(phone: &mut DuplexStream) -> Dictionary {
    let length = phone.read_u32().await.unwrap();
    assert!((1..16_384).contains(&length));
    let mut buffer = vec![0; length as usize];
    phone.read_exact(&mut buffer).await.unwrap();
    plist::from_bytes(&buffer).unwrap()
}

async fn reply(phone: &mut DuplexStream, fields: Dictionary) {
    let mut buffer = Vec::new();
    Value::Dictionary(fields)
        .to_writer_xml(&mut buffer)
        .unwrap();
    phone.write_u32(buffer.len() as u32).await.unwrap();
    phone.write_all(&buffer).await.unwrap();
}

const DEVICE_PUBLIC_KEY: &str = "-----BEGIN RSA PUBLIC KEY-----
MIIBCgKCAQEAzxKN9W1zhwU8o/3eldxBkSm6xoaWr02/GFAptdH75EG0ZEgJ+CQ0
Lbw7YY/nGfdRJUXlDdOpS0WlrrJD0uUXUzLNsKPzCVkmzS1uOYv17QHlI5R5vTDO
04prUwWgN/k+Zx1hC9mUQRrR9h6igdn3gkY9Bni3K4HQ6ZhUTBgPWLwAyZp9OX6A
4C/zYCSIQZQMAu//Hwox1Bvzs9GgbfuZ+0kmC+rlJjNXZjSDm6obsgTW5kOWLJpe
3/DrecnhBP4ipAZaXJWDzr97pYq+giVCuYAvMKNFyYN2TZca1qT7gd3JuLApTdUM
W+cejKYQuCNDRHvypMLzFCHODwW5vGmbOQIDAQAB
-----END RSA PUBLIC KEY-----\n";

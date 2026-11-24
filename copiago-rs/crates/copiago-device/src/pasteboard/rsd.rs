use std::ops::ControlFlow;

use copiago_core::{clip::PasteboardChange, i18n::Language};
use idevice::{
    RemoteXpcClient, RsdService,
    notification_proxy::NotificationProxyClient,
    rsd::RsdHandshake,
    tcp::handle::AdapterHandle,
    xpc::{Dictionary, XPCObject},
};
use plist::Value;
use tokio::time::timeout;
use tracing::{Dispatch, instrument::WithSubscriber};

use crate::{DeviceCommand, DeviceEvent};

use super::{
    METADATA_TIMEOUT, NOTIFICATIONS, PasteboardBackend, PasteboardError, PasteboardState,
    PullPolicy, SetItem,
    content::{INLINE_TEXT, PNG_UTI, Snapshot, change_id},
    pull, watch,
};

#[cfg(test)]
pub(crate) use tests::verify_push_round_trip;

pub(crate) const PASTEBOARD_SERVICE: &str = "com.apple.coredevice.pasteboardservice";
pub(super) const GENERAL_PASTEBOARD: &str = "general";

pub(crate) async fn read_rsd_types(
    adapter: &mut AdapterHandle,
    rsd: &RsdHandshake,
) -> Result<Vec<Vec<String>>, PasteboardError> {
    let port = pasteboard_port(rsd)?;
    let mut backend = RsdPasteboard { adapter, port };
    let snapshot = pull(&mut backend, PullPolicy::AllPromised).await?;
    Ok(snapshot.into_item_types())
}

pub(crate) async fn watch_rsd(
    adapter: &mut AdapterHandle,
    rsd: &mut RsdHandshake,
    wireless: bool,
    language: Language,
    state: &mut PasteboardState,
    next_command: impl AsyncFnMut() -> Option<DeviceCommand>,
    emit: impl FnMut(DeviceEvent) -> ControlFlow<()>,
) -> Result<Option<DeviceCommand>, PasteboardError> {
    let port = pasteboard_port(rsd)?;
    let mut notifications = timeout(METADATA_TIMEOUT, subscribe(adapter, rsd))
        .with_subscriber(Dispatch::none())
        .await
        .map_err(|_| PasteboardError::NotificationsUnavailable)??;

    let mut backend = RsdPasteboard { adapter, port };
    watch(
        &mut backend,
        async || {
            notifications
                .receive_notification()
                .with_subscriber(Dispatch::none())
                .await
                .map_err(|_| PasteboardError::NotificationsClosed)
        },
        wireless,
        language,
        state,
        next_command,
        emit,
    )
    .await
}

fn pasteboard_port(rsd: &RsdHandshake) -> Result<u16, PasteboardError> {
    rsd.services
        .get(PASTEBOARD_SERVICE)
        .map(|service| service.port)
        .ok_or(PasteboardError::ServiceMissing)
}

async fn subscribe(
    adapter: &mut AdapterHandle,
    rsd: &mut RsdHandshake,
) -> Result<NotificationProxyClient, PasteboardError> {
    let mut notifications = NotificationProxyClient::connect_rsd(adapter, rsd)
        .await
        .map_err(|_| PasteboardError::NotificationsUnavailable)?;
    notifications
        .observe_notification(NOTIFICATIONS[0])
        .await
        .map_err(|_| PasteboardError::NotificationsUnavailable)?;
    for name in &NOTIFICATIONS[1..] {
        let _ = notifications.observe_notification(*name).await;
    }
    Ok(notifications)
}

struct RsdPasteboard<'a> {
    adapter: &'a mut AdapterHandle,
    port: u16,
}

impl RsdPasteboard<'_> {
    async fn request(&mut self, request: XPCObject) -> Result<Value, PasteboardError> {
        // A request owns its service connection. Cancellation drops it, so a
        // late reply cannot be consumed as the next request's reply.
        async {
            let stream = self.adapter.connect(self.port).await?;
            let mut client = RemoteXpcClient::new(stream).await?;
            client.do_handshake().await?;
            client.send_object(request, true).await?;
            client.recv().await
        }
        // Upstream tracing can render the full XPC payload.
        .with_subscriber(Dispatch::none())
        .await
        .map_err(|_| PasteboardError::RequestFailed)
    }
}

impl PasteboardBackend for RsdPasteboard<'_> {
    async fn pull(&mut self, policy: PullPolicy) -> Result<Snapshot, PasteboardError> {
        // The typed client does not expose the nonce. Read the raw reply so
        // a daemon restart can be distinguished from a repeated change count.
        let reply = self.request(pull_request(policy)).await?;
        Snapshot::from_reply(reply)
    }

    async fn set(&mut self, item: SetItem) -> Result<Option<PasteboardChange>, PasteboardError> {
        let reply = self.request(set_request(item)).await?;
        set_reply(&reply)
    }
}

pub(super) fn pull_request(policy: PullPolicy) -> XPCObject {
    let (name, options) = match policy {
        PullPolicy::AllPromised => ("allPromised", Dictionary::new()),
        PullPolicy::PromiseSecondary => ("promiseSecondary", Dictionary::new()),
        PullPolicy::AllResolved => ("allResolved", Dictionary::new()),
        PullPolicy::Threshold(bytes) => (
            "thresholdData",
            Dictionary::from_iter([("bytes".to_owned(), XPCObject::Int64(bytes))]),
        ),
    };
    let policy = Dictionary::from_iter([(name.to_owned(), XPCObject::Dictionary(options))]);
    idevice::xpc!({
        "command": "PULL",
        "pasteboardName": GENERAL_PASTEBOARD,
        "dataPolicy": XPCObject::Dictionary(policy),
    })
}

fn set_request(item: SetItem) -> XPCObject {
    let entry = |uti: &str, bytes: Vec<u8>| {
        (
            uti.to_owned(),
            idevice::xpc!({ "data": XPCObject::Data(bytes) }),
        )
    };
    let (types, data): (&[&str], Dictionary) = match item {
        SetItem::Text(text) => (
            &INLINE_TEXT,
            INLINE_TEXT
                .into_iter()
                .map(|uti| entry(uti, text.as_bytes().to_vec()))
                .collect(),
        ),
        SetItem::Png(bytes) => (&[PNG_UTI], Dictionary::from_iter([entry(PNG_UTI, bytes)])),
    };
    let item = idevice::xpc!({
        "types": types.to_vec(),
        "data": XPCObject::Dictionary(data),
    });
    set_items_request(vec![item])
}

fn set_items_request(items: Vec<XPCObject>) -> XPCObject {
    idevice::xpc!({
        "command": "SET",
        "pasteboardName": GENERAL_PASTEBOARD,
        "items": XPCObject::Array(items),
        "sourceMetadata": XPCObject::Null,
    })
}

fn set_reply(reply: &Value) -> Result<Option<PasteboardChange>, PasteboardError> {
    let reply = reply
        .as_dictionary()
        .ok_or(PasteboardError::RequestFailed)?;
    if reply.get("command").and_then(Value::as_string) != Some("SET_REPLY") {
        return Err(PasteboardError::RequestFailed);
    }
    Ok(reply
        .get("pasteboard")
        .and_then(Value::as_dictionary)
        .and_then(change_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pasteboard::{
        SET_TIMEOUT, pull, read_clip,
        tests::{PUSH_TEXT, push_png},
    };
    use copiago_core::clip::Clip;
    use tokio::time::Instant;

    #[test]
    fn sends_general_pasteboard_policies_with_a_signed_threshold() {
        for (policy, name, bytes) in [
            (PullPolicy::AllPromised, "allPromised", None),
            (PullPolicy::PromiseSecondary, "promiseSecondary", None),
            (PullPolicy::AllResolved, "allResolved", None),
            (
                PullPolicy::Threshold(256 * 1024),
                "thresholdData",
                Some(256 * 1024),
            ),
        ] {
            let request = pull_request(policy);
            let request = request.as_dictionary().unwrap();
            assert_eq!(request.len(), 3);
            assert_eq!(request["command"], XPCObject::String("PULL".to_owned()));
            assert_eq!(
                request["pasteboardName"],
                XPCObject::String("general".to_owned())
            );
            let data_policy = request["dataPolicy"].as_dictionary().unwrap();
            assert_eq!(data_policy.len(), 1);
            let options = data_policy[name].as_dictionary().unwrap();
            if let Some(bytes) = bytes {
                assert_eq!(options.len(), 1);
                assert_eq!(options["bytes"], XPCObject::Int64(bytes));
            } else {
                assert!(options.is_empty());
            }
        }
    }

    #[test]
    fn set_serializes_ordered_text_representations_and_png_bytes() {
        let png = push_png();
        for (item, types, expected) in [
            (
                SetItem::Text(PUSH_TEXT.to_owned()),
                INLINE_TEXT.as_slice(),
                PUSH_TEXT.as_bytes(),
            ),
            (
                SetItem::Png(png.clone()),
                [PNG_UTI].as_slice(),
                png.as_slice(),
            ),
        ] {
            let wire = set_request(item).encode().unwrap();
            let message = XPCObject::decode(&wire).unwrap();
            let request = message.as_dictionary().unwrap();
            assert_eq!(request.len(), 4);
            assert_eq!(request["command"].as_string(), Some("SET"));
            assert_eq!(request["pasteboardName"].as_string(), Some("general"));
            assert!(matches!(request["sourceMetadata"], XPCObject::Null));
            let items = request["items"].as_array().unwrap();
            assert_eq!(items.len(), 1);
            let item = items[0].as_dictionary().unwrap();
            let ordered: Vec<_> = item["types"]
                .as_array()
                .unwrap()
                .iter()
                .map(|uti| uti.as_string().unwrap())
                .collect();
            assert_eq!(ordered, types);
            let data = item["data"].as_dictionary().unwrap();
            assert_eq!(data.len(), types.len());
            for &uti in types {
                let entry = data[uti].as_dictionary().unwrap();
                assert_eq!(entry.len(), 1);
                assert!(matches!(&entry["data"], XPCObject::Data(bytes) if bytes == expected));
            }
        }
    }

    #[test]
    fn set_reply_uses_the_same_change_id_as_a_pull() {
        let reply = |command, metadata| {
            idevice::xpc!({
                "command": command,
                "pasteboard": { "metadata": metadata },
            })
            .to_plist()
        };
        let metadata = || {
            idevice::xpc!({
                "nonce": XPCObject::Data(vec![1, 2, 3]),
                "changeCount": XPCObject::Int64(41),
            })
        };
        let push = set_reply(&reply("SET_REPLY", metadata())).unwrap().unwrap();
        let pull = Snapshot::from_reply(reply("PULL_REPLY", metadata())).unwrap();
        assert_eq!(push.count, 41);
        assert!(push.nonce.is_some() && pull.change == Some(push));
        assert!(
            set_reply(&reply("SET_REPLY", XPCObject::Null))
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            set_reply(&reply("PULL_REPLY", metadata())),
            Err(PasteboardError::RequestFailed)
        ));
    }

    pub(crate) async fn verify_push_round_trip(
        adapter: &mut AdapterHandle,
        rsd: &RsdHandshake,
    ) -> Result<(), &'static str> {
        let port = rsd
            .services
            .get(PASTEBOARD_SERVICE)
            .ok_or("pasteboard service missing")?
            .port;
        let mut backend = RsdPasteboard { adapter, port };
        if !is_empty(&mut backend).await {
            return Err("preflight failed: need a verified empty pasteboard");
        }
        // Prove that restoring the empty pasteboard works before writing test data.
        restore_empty(&mut backend).await?;

        let mut state = PasteboardState::default();
        let mut check = Ok(());
        for (kind, clip) in [
            ("text", Clip::Text(PUSH_TEXT.to_owned())),
            (
                "png",
                Clip::Image {
                    bytes: push_png(),
                    uti: PNG_UTI.to_owned(),
                },
            ),
        ] {
            let started = Instant::now();
            if state.push(&mut backend, clip.clone()).await.is_err() {
                check = Err("test SET failed");
                break;
            }
            let set_elapsed = started.elapsed();
            if !read_back_matches(&mut backend, &clip).await {
                check = Err("test PULL payload mismatch");
                break;
            }
            println!(
                "{kind}_match set_ms={} round_trip_ms={}",
                set_elapsed.as_millis(),
                started.elapsed().as_millis()
            );
        }
        restore_empty(&mut backend).await?;
        println!("restored_empty");
        check
    }

    async fn is_empty(backend: &mut RsdPasteboard<'_>) -> bool {
        let request = backend.request(pull_request(PullPolicy::AllPromised));
        let Ok(Ok(reply)) = timeout(METADATA_TIMEOUT, request).await else {
            return false;
        };
        let Some(reply) = reply.as_dictionary() else {
            return false;
        };
        let Some(board) = reply.get("pasteboard").and_then(Value::as_dictionary) else {
            return false;
        };
        reply.get("command").and_then(Value::as_string) == Some("PULL_REPLY")
            && board
                .get("items")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty)
            && board
                .get("sourceMetadata")
                .is_none_or(|value| value.as_string() == Some(""))
            && change_id(board).is_some()
    }

    async fn restore_empty(backend: &mut RsdPasteboard<'_>) -> Result<(), &'static str> {
        let request = backend.request(set_items_request(Vec::new()));
        let reply = timeout(SET_TIMEOUT, request)
            .await
            .map_err(|_| "restoration SET timed out")?
            .map_err(|_| "restoration SET failed")?;
        set_reply(&reply).map_err(|_| "restoration SET reply invalid")?;
        if !is_empty(backend).await {
            return Err("empty pasteboard restoration could not be verified");
        }
        Ok(())
    }

    async fn read_back_matches(backend: &mut RsdPasteboard<'_>, expected: &Clip) -> bool {
        let Ok(listing) = pull(backend, PullPolicy::AllPromised).await else {
            return false;
        };
        let Ok(update) = read_clip(backend, listing, Language::ZhCn).await else {
            return false;
        };
        matches!(update.event, Some(DeviceEvent::Clip { clip, .. }) if clip == *expected)
    }
}

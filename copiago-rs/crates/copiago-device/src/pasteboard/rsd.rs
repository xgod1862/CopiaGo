use std::ops::ControlFlow;

use copiago_core::i18n::Language;
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

use crate::DeviceEvent;

use super::{
    METADATA_TIMEOUT, NOTIFICATIONS, PasteboardBackend, PasteboardError, PullPolicy,
    content::Snapshot, watch,
};

pub(crate) const PASTEBOARD_SERVICE: &str = "com.apple.coredevice.pasteboardservice";
const GENERAL_PASTEBOARD: &str = "general";

pub(crate) async fn watch_rsd(
    adapter: &mut AdapterHandle,
    rsd: &mut RsdHandshake,
    wireless: bool,
    language: Language,
    emit: impl FnMut(DeviceEvent) -> ControlFlow<()>,
) -> Result<(), PasteboardError> {
    let port = rsd
        .services
        .get(PASTEBOARD_SERVICE)
        .ok_or(PasteboardError::ServiceMissing)?
        .port;
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
        emit,
    )
    .await
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
}

fn pull_request(policy: PullPolicy) -> XPCObject {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}

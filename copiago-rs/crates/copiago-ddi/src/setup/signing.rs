use std::{io::Cursor, sync::Arc};

use http_body_util::{BodyExt, Full, Limited};
use hyper::{Request, body::Bytes, client::conn::http1};
use hyper_util::rt::TokioIo;
use plist::{Dictionary, Value};
use rustls::{
    ClientConfig, RootCertStore,
    pki_types::{CertificateDer, ServerName},
};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use uuid::Uuid;

use super::StepError;
use crate::download::ImageKind;

const TSS_HOST: &str = "gs.apple.com";
const TSS_PATH: &str = "/TSS/controller?action=2";
const MAX_TSS_RESPONSE: usize = 1024 * 1024;
// Apple Inc. Root from https://www.apple.com/certificateauthority/
// This trust store is local to the fixed Apple TSS endpoint.
const APPLE_ROOT: &[u8] = include_bytes!("apple-root-ca.cer");
const TSS_CLIENT_VERSION: &str = "libauthinstall-1104.0.9";

pub(super) fn cryptex_request(
    identity: &Dictionary,
    identifiers: &Dictionary,
    nonce: &[u8],
) -> Result<Dictionary, StepError> {
    let mut request = new_request();
    request.insert("@Cryptex1,Ticket".into(), true.into());
    for key in ["Cryptex1,ChipID", "Cryptex1,ProductClass"] {
        let value = identity
            .get(key)
            .and_then(manifest_number)
            .ok_or_else(bad_manifest)?;
        request.insert(key.into(), value.into());
    }
    for key in [
        "Cryptex1,Type",
        "Cryptex1,SubType",
        "Cryptex1,UseProductClass",
        "Cryptex1,NonceDomain",
        "Cryptex1,Version",
        "Cryptex1,PreauthorizationVersion",
    ] {
        let value = identity.get(key).cloned().ok_or_else(bad_manifest)?;
        request.insert(key.into(), value);
    }

    let production = identifiers
        .get("img4_chip_cpro")
        .and_then(|value| {
            value
                .as_boolean()
                .or_else(|| value.as_unsigned_integer().map(|flag| flag != 0))
        })
        .ok_or(StepError::Kind("MissingPersonalizationIdentifier"))?;
    let mut udid = Vec::with_capacity(16);
    for key in ["img4_chip_chip", "img4_chip_ecid"] {
        udid.extend_from_slice(&identifier(identifiers, key)?.to_be_bytes());
    }
    request.insert("Cryptex1,ProductionMode".into(), production.into());
    request.insert("Cryptex1,UDID".into(), Value::Data(udid));
    request.insert("Cryptex1,Nonce".into(), Value::Data(nonce.to_vec()));
    request.insert("Cryptex1,UniqueTagList".into(), Value::Data(Vec::new()));

    for (name, component) in dictionary_at(identity, "Manifest").ok_or_else(bad_manifest)? {
        if !name.starts_with("Cryptex1,") {
            continue;
        }
        let Some(component) = component.as_dictionary() else {
            continue;
        };
        let personalize = dictionary_at(component, "Info")
            .and_then(|info| info.get("Personalize"))
            .and_then(Value::as_boolean);
        if personalize == Some(true) {
            let digest = component.get("Digest").cloned().ok_or_else(bad_manifest)?;
            let entry = Dictionary::from_iter([("Digest".to_owned(), digest)]);
            request.insert(name.clone(), Value::Dictionary(entry));
        }
    }
    Ok(request)
}

pub(super) fn personalized_request(
    build_manifest: &Dictionary,
    identifiers: &Dictionary,
    ecid: u64,
    nonce: Vec<u8>,
) -> Result<Dictionary, StepError> {
    let board = identifier(identifiers, "BoardId")?;
    let chip = identifier(identifiers, "ChipID")?;
    let identity = build_manifest
        .get("BuildIdentities")
        .and_then(Value::as_array)
        .ok_or_else(bad_manifest)?
        .iter()
        .filter_map(Value::as_dictionary)
        .find(|identity| {
            identity.get("ApBoardID").and_then(manifest_number) == Some(board)
                && identity.get("ApChipID").and_then(manifest_number) == Some(chip)
        })
        .ok_or_else(bad_manifest)?;
    let components = dictionary_at(identity, "Manifest").ok_or_else(bad_manifest)?;
    let restored_fields = personalized_restore_actions(components);

    let mut request = new_request();
    request.extend(
        identifiers
            .iter()
            .filter(|(key, _)| key.starts_with("Ap,"))
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    for key in [
        "@ApImg4Ticket",
        "@BBTicket",
        "ApProductionMode",
        "ApSecurityMode",
    ] {
        request.insert(key.into(), true.into());
    }
    for (key, value) in [
        ("ApBoardID", board),
        ("ApChipID", chip),
        ("ApECID", ecid),
        ("ApSecurityDomain", 1),
    ] {
        request.insert(key.into(), value.into());
    }
    request.insert("UID_MODE".into(), false.into());
    request.insert("ApNonce".into(), Value::Data(nonce));
    request.insert("SepNonce".into(), Value::Data(vec![0; 20]));

    for (name, component) in components {
        let Some(component) = component.as_dictionary() else {
            continue;
        };
        if component.get("Trusted").and_then(Value::as_boolean) != Some(true)
            || dictionary_at(component, "Info").is_none()
        {
            continue;
        }
        let mut entry = component.clone();
        entry.remove("Info");
        entry.extend(restored_fields.clone());
        if !component.contains_key("Digest") {
            entry.insert("Digest".into(), Value::Data(Vec::new()));
        }
        request.insert(name.clone(), Value::Dictionary(entry));
    }
    Ok(request)
}

fn personalized_restore_actions(components: &Dictionary) -> Dictionary {
    let Some(rules) = dictionary_at(components, "LoadableTrustCache")
        .and_then(|cache| dictionary_at(cache, "Info"))
        .and_then(|info| info.get("RestoreRequestRules"))
        .and_then(Value::as_array)
    else {
        return Dictionary::new();
    };
    let mut actions = Dictionary::new();
    for rule in rules.iter().filter_map(Value::as_dictionary) {
        let Some(conditions) = dictionary_at(rule, "Conditions") else {
            continue;
        };
        // These are the only enabled restore conditions for a personalized DDI.
        let applies = conditions.iter().all(|(key, value)| {
            matches!(
                key.as_str(),
                "ApRawProductionMode"
                    | "ApCurrentProductionMode"
                    | "ApRawSecurityMode"
                    | "ApRequiresImage4"
            ) && (value.as_boolean() == Some(true) || value.as_unsigned_integer() == Some(1))
        });
        if applies && let Some(fields) = dictionary_at(rule, "Actions") {
            actions.extend(
                fields
                    .iter()
                    .filter(|(_, value)| value.as_unsigned_integer() != Some(255))
                    .map(|(key, value)| (key.clone(), value.clone())),
            );
        }
    }
    actions
}

fn new_request() -> Dictionary {
    let uuid = Uuid::new_v4().to_string().to_ascii_uppercase();
    Dictionary::from_iter([
        ("@HostPlatformInfo", "mac"),
        ("@VersionInfo", TSS_CLIENT_VERSION),
        ("@UUID", uuid.as_str()),
    ])
}

fn dictionary_at<'a>(dictionary: &'a Dictionary, key: &str) -> Option<&'a Dictionary> {
    dictionary.get(key)?.as_dictionary()
}

fn manifest_number(value: &Value) -> Option<u64> {
    if let Some(text) = value.as_string() {
        if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
            return u64::from_str_radix(hex, 16).ok();
        }
        return text.parse().ok();
    }
    value.as_unsigned_integer()
}

fn identifier(ids: &Dictionary, name: &str) -> Result<u64, StepError> {
    ids.get(name)
        .and_then(Value::as_unsigned_integer)
        .ok_or(StepError::Kind("MissingPersonalizationIdentifier"))
}

fn bad_manifest() -> StepError {
    StepError::Kind("NoSuchBuildIdentityError")
}

pub(super) async fn request_ticket(
    payload: Dictionary,
    kind: ImageKind,
) -> Result<Vec<u8>, StepError> {
    let mut body = Vec::new();
    Value::Dictionary(payload)
        .to_writer_xml(&mut body)
        .map_err(|_| StepError::Kind("InvalidFileException"))?;
    let request = Request::post(TSS_PATH)
        .header("Host", TSS_HOST)
        .header("Connection", "close")
        .header("Cache-Control", "no-cache")
        .header("Content-Type", "text/xml; charset=utf-8")
        .header("User-Agent", "InetURL/1.0")
        .body(Full::new(Bytes::from(body)))
        .map_err(|_| StepError::Kind("HTTPError"))?;

    let socket = TcpStream::connect((TSS_HOST, 443)).await?;
    let tls = apple_tls()?
        .connect(
            ServerName::try_from(TSS_HOST).expect("fixed Apple DNS name"),
            socket,
        )
        .await
        .map_err(|_| StepError::Kind("SSLError"))?;
    let (mut sender, connection) = http1::handshake(TokioIo::new(tls))
        .await
        .map_err(|_| StepError::Kind("HTTPError"))?;
    let receive = async move {
        let response = sender
            .send_request(request)
            .await
            .map_err(|_| StepError::Kind("ConnectionError"))?;
        if !response.status().is_success() {
            return Err(StepError::HttpStatus(response.status().as_u16()));
        }
        Limited::new(response.into_body(), MAX_TSS_RESPONSE)
            .collect()
            .await
            .map(|body| body.to_bytes())
            .map_err(|_| StepError::Kind("HTTPError"))
    };
    // Drive HTTP and consume its response together. Cancellation drops the TCP/TLS stream.
    let (_, body) = tokio::try_join!(
        async {
            connection
                .await
                .map_err(|_| StepError::Kind("ConnectionError"))
        },
        receive,
    )?;
    decode_ticket(&body, kind)
}

fn apple_tls() -> Result<TlsConnector, StepError> {
    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from(APPLE_ROOT))
        .map_err(|_| StepError::Kind("SSLError"))?;
    let config =
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|_| StepError::Kind("SSLError"))?
            .with_root_certificates(roots)
            .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
}

pub(super) fn decode_ticket(body: &[u8], kind: ImageKind) -> Result<Vec<u8>, StepError> {
    const PAYLOAD: &[u8] = b"&REQUEST_STRING=";
    let offset = body
        .windows(PAYLOAD.len())
        .position(|window| window == PAYLOAD)
        .ok_or(StepError::Kind("TSSError"))?;
    let success = body[..offset]
        .split(|byte| *byte == b'&')
        .any(|field| field == b"MESSAGE=SUCCESS");
    if !success {
        return Err(StepError::Kind("TSSError"));
    }
    let xml = &body[offset + PAYLOAD.len()..];
    let mut response = Value::from_reader(Cursor::new(xml))
        .ok()
        .and_then(Value::into_dictionary)
        .ok_or(StepError::Kind("InvalidFileException"))?;
    let ticket_names = match kind {
        ImageKind::Personalized => &["ApImg4Ticket"][..],
        ImageKind::Cryptex => &["Cryptex1,Ticket", "ApImg4Ticket"][..],
    };
    ticket_names
        .iter()
        .find_map(|name| match response.remove(name) {
            Some(Value::Data(ticket)) if !ticket.is_empty() => Some(ticket),
            _ => None,
        })
        .ok_or(StepError::Kind("TSSError"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cryptex_uses_only_personalized_components_and_big_endian_udid() {
        let components = dict([
            ("Cryptex1,Generic", component(false)),
            ("Cryptex1,Signed", component(true)),
            ("Unrelated", component(true)),
        ]);
        let identity = dict([
            ("Cryptex1,ChipID", "0x11".into()),
            ("Cryptex1,ProductClass", 2u64.into()),
            ("Cryptex1,Type", "ddi".into()),
            ("Cryptex1,SubType", "developer".into()),
            ("Cryptex1,UseProductClass", true.into()),
            ("Cryptex1,NonceDomain", 4u64.into()),
            ("Cryptex1,Version", 5u64.into()),
            ("Cryptex1,PreauthorizationVersion", 6u64.into()),
            ("Manifest", components.into()),
        ]);
        let ids = dict([
            ("img4_chip_cpro", 1u64.into()),
            ("img4_chip_chip", 0x1234u64.into()),
            ("img4_chip_ecid", 0x5678u64.into()),
        ]);
        let request = cryptex_request(&identity, &ids, &[7, 8]).unwrap();
        assert_eq!(request["Cryptex1,ChipID"].as_unsigned_integer(), Some(17));
        assert_eq!(request["Cryptex1,ProductionMode"].as_boolean(), Some(true));
        assert_eq!(
            request["Cryptex1,UDID"].as_data().unwrap(),
            [0, 0, 0, 0, 0, 0, 0x12, 0x34, 0, 0, 0, 0, 0, 0, 0x56, 0x78]
        );
        assert_eq!(request["Cryptex1,Nonce"].as_data().unwrap(), [7, 8]);
        assert!(!request.contains_key("Cryptex1,Generic"));
        assert!(!request.contains_key("Unrelated"));
        assert_eq!(
            request["Cryptex1,Signed"].as_dictionary().unwrap(),
            &dict([("Digest", Value::Data(vec![9]))])
        );
    }

    #[test]
    fn personalized_matches_identity_and_applies_only_enabled_restore_actions() {
        let applicable = dict([
            (
                "Conditions",
                dict([("ApRequiresImage4", true.into())]).into(),
            ),
            (
                "Actions",
                dict([("Mode", "restore".into()), ("Skip", 255u64.into())]).into(),
            ),
        ]);
        let ignored = dict([
            ("Conditions", dict([("ApInRomDFU", true.into())]).into()),
            ("Actions", dict([("Mode", "ignored".into())]).into()),
        ]);
        let components = dict([
            (
                "LoadableTrustCache",
                dict([
                    ("Trusted", true.into()),
                    (
                        "Info",
                        dict([(
                            "RestoreRequestRules",
                            Value::Array(vec![applicable.into(), ignored.into()]),
                        )])
                        .into(),
                    ),
                ])
                .into(),
            ),
            (
                "Untrusted",
                dict([
                    ("Trusted", false.into()),
                    ("Info", Dictionary::new().into()),
                ])
                .into(),
            ),
        ]);
        let identity = dict([
            ("ApBoardID", "0x1".into()),
            ("ApChipID", 2u64.into()),
            ("Manifest", components.into()),
        ]);
        let manifest = dict([("BuildIdentities", Value::Array(vec![identity.into()]))]);
        let ids = dict([
            ("BoardId", 1u64.into()),
            ("ChipID", 2u64.into()),
            ("Ap,Example", 3u64.into()),
            ("Unrelated", 4u64.into()),
        ]);
        let request = personalized_request(&manifest, &ids, 5, vec![6]).unwrap();
        assert_eq!(request["ApECID"].as_unsigned_integer(), Some(5));
        assert_eq!(request["Ap,Example"].as_unsigned_integer(), Some(3));
        assert_eq!(request["ApNonce"].as_data().unwrap(), [6]);
        assert!(!request.contains_key("Unrelated"));
        assert!(!request.contains_key("Untrusted"));
        assert_eq!(
            request["LoadableTrustCache"].as_dictionary().unwrap(),
            &dict([
                ("Trusted", true.into()),
                ("Mode", "restore".into()),
                ("Digest", Value::Data(Vec::new())),
            ])
        );
    }

    fn component(personalized: bool) -> Value {
        dict([
            ("Info", dict([("Personalize", personalized.into())]).into()),
            ("Digest", Value::Data(vec![9])),
        ])
        .into()
    }

    fn dict<const N: usize>(fields: [(&str, Value); N]) -> Dictionary {
        Dictionary::from_iter(fields)
    }
}

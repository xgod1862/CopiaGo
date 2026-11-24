use super::*;
use copiago_core::clip::PasteboardChange;
use std::time::Duration;
use tokio::{io::AsyncWriteExt, time::timeout};

#[tokio::test]
async fn streams_over_both_flow_control_windows_with_destination_backpressure() {
    for method in [Method::Threshold, Method::Resolve] {
        let size = 3 * u64::from(INITIAL_WINDOW) + 3;
        let (mut writer, mut reader) = duplex(113);
        let verify = async {
            let mut sha = Sha256::new();
            let mut block = [0; 513];
            let mut remaining = size;
            while remaining != 0 {
                let count = remaining.min(block.len() as u64) as usize;
                reader.read_exact(&mut block[..count]).await.unwrap();
                sha.update(&block[..count]);
                remaining -= count as u64;
                tokio::task::yield_now().await;
            }
            <[u8; 32]>::from(sha.finalize())
        };
        let (result, digest) = timeout(Duration::from_secs(10), async {
            tokio::join!(run(method, size, &mut writer), verify)
        })
        .await
        .expect("flow-control windows or sink stalled");
        assert_eq!(result, Ok((size, digest)));
    }
}

#[test]
fn synthetic_envelopes_match_idevice_object_decoding() {
    for method in [Method::Threshold, Method::Resolve] {
        let bytes = [17, 29, 41, 53, 65];
        let (mut wire, suffix) = envelope(method, bytes.len() as u64);
        wire.extend_from_slice(&bytes);
        wire.extend_from_slice(&suffix);
        let message = XPCMessage::decode(&wire)
            .unwrap()
            .message
            .unwrap()
            .to_plist();
        let fields = message.as_dictionary().unwrap();
        let data = match method {
            Method::Threshold => {
                let board = fields["pasteboard"].as_dictionary().unwrap();
                let item = board["items"].as_array().unwrap()[1]
                    .as_dictionary()
                    .unwrap();
                let representations = item["data"].as_dictionary().unwrap();
                &representations[SAMPLE_UTI].as_dictionary().unwrap()["data"]
            }
            Method::Resolve => &fields["data"],
        };
        assert_eq!(data.as_data(), Some(bytes.as_slice()));
    }
}

#[tokio::test]
async fn rejects_truncation_after_data_even_when_all_payload_bytes_were_written() {
    for method in [Method::Threshold, Method::Resolve] {
        let bytes = b"synthetic";
        let (mut wire, _) = envelope(method, bytes.len() as u64);
        wire.extend_from_slice(bytes);
        let (result, staged) = receive_wire(method, &wire, Some(bytes.len() as u64), true).await;
        assert_eq!(result, Err(Error::Truncated));
        assert_eq!(staged, bytes);
        let (result, staged) = receive_wire(method, &wire, Some(bytes.len() as u64), false).await;
        assert_eq!(result, Err(Error::Truncated));
        assert_eq!(staged, bytes);
    }
}

#[tokio::test]
async fn rejects_payload_over_limit_or_different_size_before_writing() {
    for method in [Method::Threshold, Method::Resolve] {
        let (wire, _) = envelope(method, super::super::MAX_FILE_BYTES + 1);
        let (result, staged) = receive_wire(method, &wire, None, true).await;
        assert_eq!(result, Err(Error::TooLarge));
        assert!(staged.is_empty());

        let (wire, _) = envelope(method, 10);
        let (result, staged) = receive_wire(method, &wire, Some(9), true).await;
        assert_eq!(result, Err(Error::SizeMismatch));
        assert!(staged.is_empty());
    }
}

#[tokio::test]
async fn distinguishes_unavailable_data_and_an_out_of_line_stream() {
    for (data, expected) in [
        (XPCObject::Null, Error::DataUnavailable),
        (
            XPCObject::FileTransfer {
                msg_id: 1,
                data: Box::new(XPCObject::Null),
            },
            Error::UnsupportedPayload,
        ),
    ] {
        let wire = XPCMessage {
            flags: 0x0002_0101,
            message: Some(idevice::xpc!({ "command": "DATA", "data": data })),
            message_id: Some(1),
        }
        .encode(1)
        .unwrap();
        let (result, staged) = receive_wire(Method::Resolve, &wire, None, true).await;
        assert_eq!(result, Err(expected));
        assert!(staged.is_empty());
    }
}

#[tokio::test]
async fn consumes_padded_frames_and_rejects_wrong_command_after_data() {
    let (mut wire, suffix) = envelope(Method::Resolve, 3);
    wire.extend_from_slice(b"123");
    wire.extend_from_slice(&suffix);
    assert_eq!(
        receive_wire(Method::Resolve, &wire, Some(3), true).await,
        (Ok(3), b"123".to_vec())
    );
    let position = wire.windows(4).rposition(|word| word == b"DATA").unwrap();
    wire[position..position + 4].copy_from_slice(b"PUSH");
    let (result, staged) = receive_wire(Method::Resolve, &wire, Some(3), true).await;
    assert_eq!(result, Err(Error::InvalidReply));
    assert_eq!(staged, b"123");
}

#[tokio::test]
async fn rejects_threshold_reply_for_a_newer_change_after_staging_bytes() {
    let message = idevice::xpc!({
        "command": "PULL_REPLY",
        "pasteboard": {
            "items": XPCObject::Array(vec![idevice::xpc!({
                "data": { "public.zip-archive": { "data": XPCObject::Data(vec![1, 2, 3]) } },
            })]),
            "metadata": { "changeCount": 42i64 },
        },
    });
    let wire = XPCMessage {
        flags: 0x0002_0101,
        message: Some(message),
        message_id: Some(1),
    }
    .encode(1)
    .unwrap();
    let change = PasteboardChange {
        count: 41,
        nonce: None,
    };
    let (result, staged) =
        receive_wire_with_change(Method::Threshold, &wire, Some(3), true, Some(&change)).await;
    assert_eq!(result, Err(Error::OfferExpired));
    assert_eq!(staged, [1, 2, 3]);
}

#[test]
fn small_replies_validate_object_allocation_lengths() {
    let mut wire = XPCObject::Data(vec![1, 2, 3]).encode().unwrap();
    assert!(super::super::reply::decode_small(&wire).is_ok());
    wire[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(
        super::super::reply::decode_small(&wire),
        Err(Error::InvalidReply)
    );
}

#[tokio::test(start_paused = true)]
async fn a_stalled_reply_times_out_before_a_staged_file_can_be_committed() {
    let (mut consumer, mut producer) = duplex(4096);
    let (mut prefix, _) = envelope(Method::Resolve, 3);
    prefix.extend_from_slice(b"abc");
    let mut out = Vec::new();
    let client = timeout(
        Duration::from_secs(1),
        receive(
            &mut consumer,
            Method::Resolve,
            SAMPLE_UTI,
            SAMPLE_ITEM,
            Some(3),
            None,
            &mut out,
        ),
    );
    let server = async {
        let request = fetch_request(Method::Resolve, SAMPLE_UTI, SAMPLE_ITEM, Some(3));
        Peer::handshake(&mut producer, request).await.unwrap();
        write_frame(&mut producer, 0, 0, REPLY, &prefix)
            .await
            .unwrap();
    };
    let (timed, ()) = tokio::join!(client, server);
    assert!(timed.is_err());
    assert_eq!(out, b"abc");
}

async fn receive_wire(
    method: Method,
    wire: &[u8],
    size: Option<u64>,
    end_stream: bool,
) -> (Result<u64, Error>, Vec<u8>) {
    receive_wire_with_change(method, wire, size, end_stream, None).await
}

async fn receive_wire_with_change(
    method: Method,
    wire: &[u8],
    size: Option<u64>,
    end_stream: bool,
    change: Option<&PasteboardChange>,
) -> (Result<u64, Error>, Vec<u8>) {
    let (mut client, mut server) = duplex(4096);
    let mut output = Vec::new();
    let client = receive(
        &mut client,
        method,
        SAMPLE_UTI,
        SAMPLE_ITEM,
        size,
        change,
        &mut output,
    );
    let server = async {
        let request = fetch_request(method, SAMPLE_UTI, SAMPLE_ITEM, size);
        Peer::handshake(&mut server, request).await.unwrap();
        let mut padded = vec![2];
        padded.extend_from_slice(wire);
        padded.extend_from_slice(&[0; 2]);
        write_frame(&mut server, 0, 8 | u8::from(end_stream), REPLY, &padded)
            .await
            .unwrap();
        server.shutdown().await.unwrap();
    };
    let (result, ()) = tokio::join!(client, server);
    (result, output)
}

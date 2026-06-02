//! Topic-prefixed msgpack+zstd frame codec (#431) — wire-compatible with the
//! noaide console client (`frontend/src/transport/codec.ts:31-52`).
//!
//! Frame layout (all integers big-endian):
//! ```text
//! [2B topic_len][topic UTF-8][1B codec_id][4B payload_len][payload]
//! ```
//! - `codec_id` = `0x01` (msgpack)
//! - `payload`  = zstd-compressed msgpack bytes; `payload_len` is the COMPRESSED length
//!
//! UUIDs serialize as 16 raw bytes (rmp-serde is non-human-readable) — the client
//! maps the 16-byte array back to a hyphenated string (`codec.ts:93-107`).

use serde::Serialize;

/// msgpack codec id (matches noaide `CODEC_MSGPACK`).
pub const CODEC_MSGPACK: u8 = 0x01;
/// zstd compression level (matches `sentinel-console-plane`).
const ZSTD_LEVEL: i32 = 3;

/// Header size after the topic bytes: `codec_id` (1) + `payload_len` (4).
const HEADER_AFTER_TOPIC: usize = 5;

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("msgpack encode: {0}")]
    Encode(#[from] rmp_serde::encode::Error),
    #[error("msgpack decode: {0}")]
    Decode(#[from] rmp_serde::decode::Error),
    #[error("zstd: {0}")]
    Zstd(#[from] std::io::Error),
    #[error("topic too long: {0} bytes (max 65535)")]
    TopicTooLong(usize),
    #[error("frame truncated: need {need} bytes, have {have}")]
    Truncated { need: usize, have: usize },
    #[error("unknown codec id: {0:#04x}")]
    UnknownCodec(u8),
}

/// Encodes a value into a topic-prefixed msgpack+zstd frame.
pub fn encode_frame<T: Serialize>(topic: &str, value: &T) -> Result<Vec<u8>, CodecError> {
    let topic_bytes = topic.as_bytes();
    if topic_bytes.len() > u16::MAX as usize {
        return Err(CodecError::TopicTooLong(topic_bytes.len()));
    }
    let msgpack = rmp_serde::to_vec_named(value)?;
    let compressed = zstd::encode_all(msgpack.as_slice(), ZSTD_LEVEL)?;

    let mut frame =
        Vec::with_capacity(2 + topic_bytes.len() + HEADER_AFTER_TOPIC + compressed.len());
    frame.extend_from_slice(&(topic_bytes.len() as u16).to_be_bytes());
    frame.extend_from_slice(topic_bytes);
    frame.push(CODEC_MSGPACK);
    frame.extend_from_slice(&(compressed.len() as u32).to_be_bytes());
    frame.extend_from_slice(&compressed);
    Ok(frame)
}

/// A decoded frame: topic + the raw (decompressed) msgpack bytes still to be deserialized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedFrame {
    pub topic: String,
    pub msgpack: Vec<u8>,
}

/// Parses a frame into `(topic, decompressed msgpack bytes)`. Mirror of `encode_frame`;
/// used by the integration test client and any in-process consumer.
pub fn decode_frame(data: &[u8]) -> Result<DecodedFrame, CodecError> {
    if data.len() < 2 {
        return Err(CodecError::Truncated {
            need: 2,
            have: data.len(),
        });
    }
    let topic_len = u16::from_be_bytes([data[0], data[1]]) as usize;
    let after_topic = 2 + topic_len;
    if data.len() < after_topic + HEADER_AFTER_TOPIC {
        return Err(CodecError::Truncated {
            need: after_topic + HEADER_AFTER_TOPIC,
            have: data.len(),
        });
    }
    let topic = String::from_utf8_lossy(&data[2..after_topic]).into_owned();
    let codec_id = data[after_topic];
    if codec_id != CODEC_MSGPACK {
        return Err(CodecError::UnknownCodec(codec_id));
    }
    let len_off = after_topic + 1;
    let payload_len = u32::from_be_bytes([
        data[len_off],
        data[len_off + 1],
        data[len_off + 2],
        data[len_off + 3],
    ]) as usize;
    let payload_off = len_off + 4;
    if data.len() < payload_off + payload_len {
        return Err(CodecError::Truncated {
            need: payload_off + payload_len,
            have: data.len(),
        });
    }
    let compressed = &data[payload_off..payload_off + payload_len];
    let msgpack = zstd::decode_all(compressed)?;
    Ok(DecodedFrame { topic, msgpack })
}

/// Convenience: decode a frame straight into a typed value.
pub fn decode_frame_as<T: serde::de::DeserializeOwned>(
    data: &[u8],
) -> Result<(String, T), CodecError> {
    let f = decode_frame(data)?;
    let value = rmp_serde::from_slice(&f.msgpack)?;
    Ok((f.topic, value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct Sample {
        id: u32,
        name: String,
        values: Vec<f64>,
    }

    #[test]
    fn roundtrip_topic_and_value() {
        let v = Sample {
            id: 7,
            name: "agent-01".into(),
            values: vec![1.0, 2.5, 3.25],
        };
        let frame = encode_frame("agent_live", &v).unwrap();
        let (topic, decoded): (String, Sample) = decode_frame_as(&frame).unwrap();
        assert_eq!(topic, "agent_live");
        assert_eq!(decoded, v);
    }

    #[test]
    fn frame_layout_is_noaide_compatible() {
        // [2B topic_len][topic][1B 0x01][4B payload_len][zstd payload]
        let frame = encode_frame("hi", &"x").unwrap();
        assert_eq!(u16::from_be_bytes([frame[0], frame[1]]), 2, "topic_len");
        assert_eq!(&frame[2..4], b"hi", "topic bytes");
        assert_eq!(frame[4], CODEC_MSGPACK, "codec id 0x01");
        let payload_len = u32::from_be_bytes([frame[5], frame[6], frame[7], frame[8]]) as usize;
        assert_eq!(
            frame.len(),
            9 + payload_len,
            "total = header + compressed payload"
        );
        // The compressed payload must zstd-decode back to valid msgpack.
        let decoded = decode_frame(&frame).unwrap();
        assert_eq!(decoded.topic, "hi");
        assert!(!decoded.msgpack.is_empty());
    }

    #[test]
    fn truncated_frame_errors_not_panics() {
        let frame = encode_frame("topic", &42u32).unwrap();
        assert!(matches!(
            decode_frame(&frame[..3]),
            Err(CodecError::Truncated { .. })
        ));
    }

    #[test]
    fn unknown_codec_id_rejected() {
        let mut frame = encode_frame("t", &1u8).unwrap();
        frame[3] = 0xFF; // corrupt codec id (after 2B len + 1B topic "t")
        assert!(matches!(
            decode_frame(&frame),
            Err(CodecError::UnknownCodec(0xFF))
        ));
    }
}

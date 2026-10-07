//! Strict ID3v2.3/v2.4 PRIV + ADTS AAC-LC input.
use super::*;
use crate::codecs::aac;

fn syncsafe(bytes: &[u8]) -> Result<usize> {
    if bytes.len() != 4 || bytes.iter().any(|v| v & 0x80 != 0) {
        return Err(Error::bitstream("invalid ID3 syncsafe size"));
    }
    Ok(bytes.iter().fold(0, |n, b| (n << 7) | usize::from(*b)))
}

/// Keep the 33-bit clock and exact sample-count offset separate until epoch mapping.
struct PackedFrame<'a> {
    anchor_90k: u64,
    offset_samples: u64,
    sample_rate: u32,
    payload: &'a [u8],
    header: aac::AdtsHeader,
    offset: usize,
}
fn frames(bytes: &[u8]) -> Result<Vec<PackedFrame<'_>>> {
    let invalid = || Error::bitstream("invalid Packed AAC ID3 anchor");
    if bytes.len() < 10
        || &bytes[..3] != b"ID3"
        || !matches!(bytes[3], 3 | 4)
        || bytes[4] != 0
        || bytes[5] != 0
    {
        return Err(invalid());
    }
    let tag_end = 10usize
        .checked_add(syncsafe(&bytes[6..10])?)
        .ok_or_else(invalid)?;
    if tag_end > bytes.len() {
        return Err(invalid());
    }
    let mut cursor = 10;
    let mut anchor = None;
    while cursor < tag_end {
        if bytes[cursor] == 0 {
            if !bytes[cursor..tag_end].iter().all(|v| *v == 0) {
                return Err(invalid());
            }
            break;
        }
        if tag_end - cursor < 10 {
            return Err(invalid());
        }
        let header = &bytes[cursor..cursor + 10];
        if !header[..4]
            .iter()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        {
            return Err(invalid());
        }
        let size = if bytes[3] == 4 {
            syncsafe(&header[4..8])?
        } else {
            u32::from_be_bytes(header[4..8].try_into().unwrap()) as usize
        };
        if header[8..10] != [0, 0] {
            return Err(invalid());
        }
        cursor += 10;
        let end = cursor
            .checked_add(size)
            .filter(|v| *v <= tag_end)
            .ok_or_else(invalid)?;
        if &header[..4] == b"PRIV" {
            let frame = &bytes[cursor..end];
            let split = frame.iter().position(|v| *v == 0).ok_or_else(invalid)?;
            let owner = std::str::from_utf8(&frame[..split]).map_err(|_| invalid())?;
            if owner == "com.apple.streaming.transportStreamTimestamp" {
                let timestamp = super::id3_timestamp(owner, &frame[split + 1..])?;
                if anchor.replace(timestamp).is_some() {
                    return Err(invalid());
                }
            }
        }
        cursor = end;
    }
    let anchor = anchor.ok_or_else(invalid)?;
    cursor = tag_end;
    let mut result = Vec::new();
    let mut config = None;
    let mut offset_samples = 0u64;
    while cursor < bytes.len() {
        // The prototype counts exactly one raw_data_block (1024 samples).
        // Do not silently accept a layer or block layout it cannot account for.
        if bytes.get(cursor + 1).is_none_or(|b| b & 6 != 0)
            || bytes.get(cursor + 6).is_none_or(|b| b & 3 != 0)
        {
            return Err(invalid());
        }
        let header = aac::parse_adts_header(&bytes[cursor..])?;
        if header.frame_length == header.header_length {
            return Err(invalid());
        }
        let identity = (
            header.sample_rate,
            header.channel_config,
            header.audio_object_type,
        );
        if config.replace(identity).is_some_and(|old| old != identity) {
            return Err(invalid());
        }
        let end = cursor
            .checked_add(header.frame_length)
            .filter(|v| *v <= bytes.len())
            .ok_or_else(invalid)?;
        if result.len() >= 65_536 {
            return Err(Error::unsupported("Packed AAC sample budget exceeded"));
        }
        result.push(PackedFrame {
            anchor_90k: anchor,
            offset_samples,
            sample_rate: header.sample_rate,
            payload: &bytes[cursor + header.header_length..end],
            header,
            offset: cursor + header.header_length,
        });
        offset_samples = offset_samples.checked_add(1024).ok_or_else(invalid)?;
        cursor = end;
    }
    if result.is_empty() {
        return Err(invalid());
    }
    Ok(result)
}

#[test]
fn packed_clock_does_not_accumulate_per_frame_rounding() {
    let mut private = b"com.apple.streaming.transportStreamTimestamp\0".to_vec();
    private.extend_from_slice(&((1u64 << 33) - 1).to_be_bytes());
    let mut bytes = b"ID3\x04\0\0\0\0\0\0".to_vec();
    bytes[9] = (private.len() + 10) as u8;
    bytes.extend_from_slice(b"PRIV\0\0\0");
    bytes.push(private.len() as u8);
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&private);
    // AAC-LC 44.1 kHz stereo, 9-byte ADTS frame, 2-byte dummy payload.
    for _ in 0..1000 {
        bytes.extend_from_slice(&[0xff, 0xf1, 0x50, 0x80, 0x01, 0x3f, 0xfc, 1, 2]);
    }
    let parsed = frames(&bytes).unwrap();
    assert_eq!(parsed.len(), 1000);
    assert_eq!(parsed[999].anchor_90k, (1u64 << 33) - 1);
    assert_eq!(parsed[999].offset_samples, 999 * 1024);
    assert_eq!(parsed[999].sample_rate, 44_100);
    assert_eq!(parsed[999].payload, [1, 2]);
    bytes.pop();
    assert!(frames(&bytes).is_err());
    bytes[5] = 0x80;
    assert!(frames(&bytes).is_err());
}

#[cfg(test)]
fn fixture(version: u8, clock: u64) -> Vec<u8> {
    let mut private = b"com.apple.streaming.transportStreamTimestamp\0".to_vec();
    private.extend_from_slice(&clock.to_be_bytes());
    let mut bytes = vec![
        b'I',
        b'D',
        b'3',
        version,
        0,
        0,
        0,
        0,
        0,
        (private.len() + 10) as u8,
    ];
    bytes.extend_from_slice(b"PRIV\0\0\0");
    bytes.push(private.len() as u8);
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&private);
    bytes.extend_from_slice(&[0xff, 0xf1, 0x50, 0x80, 0x01, 0x3f, 0xfc, 1, 2]);
    bytes
}

#[test]
fn packed_rejects_truncated_headers_clocks_and_frames() {
    for version in [3, 4] {
        let bytes = fixture(version, 123);
        assert_eq!(frames(&bytes).unwrap()[0].anchor_90k, 123);
        for end in 0..bytes.len() {
            assert!(
                frames(&bytes[..end]).is_err(),
                "v{version} truncation at {end}"
            );
        }
    }
}

#[test]
fn packed_rejects_malformed_id3_and_unsupported_adts_layouts() {
    let original = fixture(4, 123);
    let tag_end = 10 + usize::from(original[9]);
    for (offset, value) in [
        (3, 2),
        (4, 1),
        (5, 0x80),
        (6, 0x80),
        (9, 127),
        (10, b'p'),
        (14, 0x80),
        (17, 127),
        (18, 1),
        (19, 1),
        (20, b'x'),       // wrong PRIV owner: there is no valid clock
        (tag_end - 8, 1), // more than 33 timestamp bits
        (tag_end, 0),
        (tag_end + 1, 0xf7), // sync / nonzero layer
        (tag_end + 2, 0x7c), // reserved sample rate
        (tag_end + 6, 0xfd), // two raw_data_blocks
    ] {
        let mut bytes = original.clone();
        bytes[offset] = value;
        assert!(frames(&bytes).is_err(), "accepted mutation at {offset}");
    }
    let mut duplicate = original[..tag_end].to_vec();
    duplicate.extend_from_slice(&original[10..tag_end]);
    duplicate[9] = ((tag_end - 10) * 2) as u8;
    duplicate.extend_from_slice(&original[tag_end..]);
    assert!(frames(&duplicate).is_err());
    let mut changed = original.clone();
    let mut second = original[tag_end..].to_vec();
    second[2] = 0x4c; // 48 kHz rather than 44.1 kHz
    changed.extend_from_slice(&second);
    assert!(frames(&changed).is_err());
}

/// Keep the clock exact in an LCM timescale, including 44.1 kHz samples.
pub(crate) async fn demux(
    bytes: &[u8],
    mut hook: Option<&mut dyn RawSampleHook>,
    check: &SampleCheck<'_>,
) -> Result<crate::types::DemuxOutput> {
    use crate::types::{DemuxOutput, EncodedPacket, PacketTiming};
    check()?;
    let frames = frames(bytes)?;
    let limits = hook
        .as_ref()
        .map_or((65_536, 32 * 1024 * 1024), |h| h.limits());
    if frames.len() > limits.0 || bytes.len() > limits.1 {
        return Err(Error::unsupported("Packed AAC sample budget exceeded"));
    }
    let first = &frames[0];
    let rate = first.sample_rate;
    let mut a = rate;
    let mut b = 90_000;
    while b != 0 {
        (a, b) = (b, a % b);
    }
    let timescale = (rate / a)
        .checked_mul(90_000)
        .ok_or_else(|| Error::bitstream("Packed AAC clock overflow"))?;
    let mut result = DemuxOutput {
        saw_audio: true,
        audio_timescale: Some(rate),
        sample_rate: Some(rate),
        channel_count: Some(first.header.channel_config),
        audio_specific_config: Some(aac::audio_specific_config(first.header)),
        packed_anchor: Some(first.anchor_90k),
        ..Default::default()
    };
    for frame in frames {
        check()?;
        let dts = i128::from(frame.anchor_90k) * i128::from(timescale / 90_000)
            + i128::from(frame.offset_samples) * i128::from(timescale / rate);
        let payload = if let Some(hook) = hook.as_deref_mut() {
            hook.process(RawSample {
                kind: StreamKind::Aac,
                layout: RawLayout::Adts {
                    offset: frame.offset,
                },
                dts,
                timescale,
                bytes: frame.payload.to_vec(),
                protection: None,
            })
            .await?
        } else {
            frame.payload.to_vec()
        };
        check()?;
        if payload.len() != frame.payload.len() {
            return Err(Error::bitstream("Packed AAC hook changed frame length"));
        }
        result.packets.push(EncodedPacket {
            kind: StreamKind::Aac,
            timing: Some(PacketTiming {
                edit_offset: 0,
                timescale,
                dts,
                pts: dts,
                duration: 1024 * (timescale / rate),
            }),
            data: payload,
            pts_90k: dts * 90_000 / i128::from(timescale),
            dts_90k: (dts * 90_000 / i128::from(timescale)) as u64,
            duration: 1024,
            is_key: true,
            is_length_prefixed: false,
        });
    }
    Ok(result)
}

pub(crate) fn validate(bytes: &[u8]) -> Result<()> {
    frames(bytes).map(|_| ())
}

//! Turn the two-second TS fixture into a continuous test sequence.
#![allow(dead_code)]
pub fn shift_ts(bytes: Vec<u8>, ticks: u64) -> Vec<u8> {
    shift_ts_by(bytes, ticks, ticks)
}

/// The fixture contains 60 video frames and 95 AAC frames. Join each track
/// continuously rather than introducing gaps or repeating timestamps.
pub fn continuous_ts(bytes: Vec<u8>, index: usize) -> Vec<u8> {
    shift_ts_by(bytes, index as u64 * 180_000, index as u64 * 182_400)
}
fn shift_ts_by(mut bytes: Vec<u8>, video_ticks: u64, audio_ticks: u64) -> Vec<u8> {
    for packet in bytes.as_chunks_mut::<188>().0 {
        if packet[1] & 0x40 == 0 {
            continue;
        }
        let control = (packet[3] >> 4) & 3;
        let offset = match control {
            1 => 4,
            3 => 5 + packet[4] as usize,
            _ => continue,
        };
        if offset + 14 > packet.len() || packet[offset..offset + 3] != [0, 0, 1] {
            continue;
        }
        let ticks = if (0xc0..=0xdf).contains(&packet[offset + 3]) {
            audio_ticks
        } else {
            video_ticks
        };
        let flags = (packet[offset + 7] >> 6) & 3;
        let count = match flags {
            2 => 1,
            3 => 2,
            _ => 0,
        };
        for i in 0..count {
            let start = offset + 9 + i * 5;
            let p = &mut packet[start..start + 5];
            let value = ((u64::from(p[0] >> 1) & 7) << 30)
                | (u64::from(p[1]) << 22)
                | ((u64::from(p[2] >> 1) & 127) << 15)
                | (u64::from(p[3]) << 7)
                | u64::from(p[4] >> 1);
            let value = (value + ticks) & ((1 << 33) - 1);
            p[0] = (p[0] & 0xf1) | (((value >> 30) & 7) as u8) << 1;
            p[1] = (value >> 22) as u8;
            p[2] = (((value >> 15) & 127) as u8) << 1 | 1;
            p[3] = (value >> 7) as u8;
            p[4] = ((value & 127) as u8) << 1 | 1;
        }
    }
    bytes
}
pub fn segment_index(location: &hls_engine::legacy::SourceLocation) -> usize {
    let path = match location {
        hls_engine::legacy::SourceLocation::File(p) => p.to_string_lossy().into_owned(),
        hls_engine::legacy::SourceLocation::Url(u) => u.path().to_owned(),
    };
    path.rsplit('/')
        .next()
        .unwrap()
        .strip_prefix("segment-")
        .and_then(|v| v.strip_suffix(".ts"))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// Zeros out `creation_time` and `modification_time` fields in `mvhd` and
/// `mdhd` and `tkhd` boxes within the `moov` box. These fields use wall-clock time
/// (`SystemTime::now()` in `mp4.rs`), so they differ between outputs
/// produced at different times. Normalizing them allows byte-level
/// comparison of structurally identical files.
pub fn normalize_moov_timestamps(data: &mut [u8]) {
    walk_and_zero_timestamps(data, 0, data.len());
}

fn walk_and_zero_timestamps(data: &mut [u8], start: usize, end: usize) {
    let mut offset = start;
    while offset + 8 <= end {
        let size = u32::from_be_bytes(data[offset..offset + 4].try_into().unwrap());
        let (size, header) = if size == 1 {
            if offset + 16 > end {
                break;
            }
            let Ok(size) = usize::try_from(u64::from_be_bytes(
                data[offset + 8..offset + 16].try_into().unwrap(),
            )) else {
                break;
            };
            (size, 16)
        } else {
            (size as usize, 8)
        };
        let Some(box_end) = offset
            .checked_add(size)
            .filter(|&box_end| size >= header && box_end <= end)
        else {
            break;
        };
        let payload = offset + header;
        let btype = &data[offset + 4..offset + 8];
        match btype {
            b"mvhd" | b"mdhd" | b"tkhd" => {
                // FullBox fields precede two u32 (v0) or two u64 (v1) timestamps.
                if payload + 4 <= box_end {
                    let width = match data[payload] {
                        0 => 8,
                        1 => 16,
                        _ => 0,
                    };
                    if payload + 4 + width <= box_end {
                        data[payload + 4..payload + 4 + width].fill(0);
                    }
                }
            }
            b"moov" | b"trak" | b"mdia" => {
                walk_and_zero_timestamps(data, payload, box_end);
            }
            _ => {}
        }
        offset = box_end;
    }
}

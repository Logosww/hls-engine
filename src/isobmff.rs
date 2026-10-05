use std::collections::HashMap;

use crate::codecs::{avc, hevc};
use crate::error::{Error, Result};
use crate::types::{DemuxOutput, EncodedPacket, StreamKind};

// === Box header helpers ===

struct BoxHeader {
    box_type: [u8; 4],
    header_size: usize,
    total_size: usize,
}

fn read_box_header(data: &[u8]) -> Result<BoxHeader> {
    if data.len() < 8 {
        return Err(Error::bitstream("ISOBMFF box header is too short"));
    }
    let size = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let box_type = [data[4], data[5], data[6], data[7]];
    let (header_size, total_size) = if size == 0 {
        (8, data.len())
    } else if size == 1 {
        if data.len() < 16 {
            return Err(Error::bitstream("64-bit ISOBMFF box header is too short"));
        }
        let large = usize::try_from(u64::from_be_bytes([
            data[8], data[9], data[10], data[11], data[12], data[13], data[14], data[15],
        ]))
        .map_err(|_| Error::bitstream("box size exceeds address space"))?;
        (16, large)
    } else {
        (8, size)
    };
    if total_size < header_size {
        return Err(Error::bitstream("ISOBMFF box size is smaller than header"));
    }
    Ok(BoxHeader {
        box_type,
        header_size,
        total_size,
    })
}

fn find_box<'a>(data: &'a [u8], box_type: &[u8; 4]) -> Result<Option<&'a [u8]>> {
    let mut offset = 0;
    while offset < data.len() {
        let header = read_box_header(&data[offset..])?;
        if offset
            .checked_add(header.total_size)
            .is_none_or(|end| end > data.len())
        {
            return Err(Error::bitstream("ISOBMFF box extends past data"));
        }
        if &header.box_type == box_type {
            return Ok(Some(
                &data[offset + header.header_size..offset + header.total_size],
            ));
        }
        offset += header.total_size;
    }
    Ok(None)
}

fn for_each_box<F>(data: &[u8], mut f: F) -> Result<()>
where
    F: FnMut(&[u8; 4], &[u8]) -> Result<()>,
{
    let mut offset = 0;
    while offset < data.len() {
        let header = read_box_header(&data[offset..])?;
        if offset
            .checked_add(header.total_size)
            .is_none_or(|end| end > data.len())
        {
            return Err(Error::bitstream("ISOBMFF box extends past data"));
        }
        f(
            &header.box_type,
            &data[offset + header.header_size..offset + header.total_size],
        )?;
        offset += header.total_size;
    }
    Ok(())
}

// === Init segment parsing ===

struct InitTrack {
    track_id: u32,
    kind: StreamKind,
    timescale: u32,
    timeline_offset: i128,
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
    vps: Option<Vec<u8>>,
    width: Option<u16>,
    height: Option<u16>,
    audio_specific_config: Option<Vec<u8>>,
    sample_rate: Option<u32>,
    channel_count: Option<u8>,
    default_sample_duration: u32,
    default_sample_size: u32,
    default_sample_flags: u32,
    length_size: usize,
    #[cfg(not(target_arch = "wasm32"))]
    video_codec: Option<crate::mp4::VideoCodec>,
}

fn parse_init_segment(init: &[u8]) -> Result<Vec<InitTrack>> {
    let moov = find_box(init, b"moov")?
        .ok_or_else(|| Error::bitstream("init segment does not contain a moov box"))?;

    let mut trex_defaults: HashMap<u32, (u32, u32, u32)> = HashMap::new();
    if let Some(mvex) = find_box(moov, b"mvex")? {
        for_each_box(mvex, |box_type, payload| {
            if box_type == b"trex" {
                let trex = parse_trex(payload)?;
                trex_defaults.insert(trex.0, (trex.1, trex.2, trex.3));
            }
            Ok(())
        })?;
    }

    let mvhd = find_box(moov, b"mvhd")?.ok_or_else(|| Error::bitstream("missing mvhd"))?;
    let movie_timescale = parse_mdhd_timescale(mvhd)?;
    let mut tracks = Vec::new();
    for_each_box(moov, |box_type, payload| {
        if box_type == b"trak" {
            let track = parse_trak(payload, &trex_defaults, movie_timescale)?;
            if track.track_id == 0
                || tracks
                    .iter()
                    .any(|existing: &InitTrack| existing.track_id == track.track_id)
            {
                return Err(Error::bitstream("zero or duplicate track ID"));
            }
            tracks.push(track);
        }
        Ok(())
    })?;

    if tracks
        .iter()
        .filter(|t| matches!(t.kind, StreamKind::Avc | StreamKind::Hevc))
        .count()
        > 1
        || tracks
            .iter()
            .filter(|t| matches!(t.kind, StreamKind::Aac))
            .count()
            > 1
    {
        return Err(Error::unsupported("multiple tracks of the same media kind"));
    }
    if tracks.is_empty() {
        return Err(Error::bitstream(
            "init segment does not contain any trak boxes",
        ));
    }
    Ok(tracks)
}

fn parse_trex(payload: &[u8]) -> Result<(u32, u32, u32, u32)> {
    // full box: version(1) + flags(3) = 4 bytes, then 5 × u32
    if payload.len() < 4 + 20 {
        return Err(Error::bitstream("trex box is too short"));
    }
    let pos = 4;
    let track_id = u32::from_be_bytes([
        payload[pos],
        payload[pos + 1],
        payload[pos + 2],
        payload[pos + 3],
    ]);
    let default_sample_duration = u32::from_be_bytes([
        payload[pos + 8],
        payload[pos + 9],
        payload[pos + 10],
        payload[pos + 11],
    ]);
    let default_sample_size = u32::from_be_bytes([
        payload[pos + 12],
        payload[pos + 13],
        payload[pos + 14],
        payload[pos + 15],
    ]);
    let default_sample_flags = u32::from_be_bytes([
        payload[pos + 16],
        payload[pos + 17],
        payload[pos + 18],
        payload[pos + 19],
    ]);
    Ok((
        track_id,
        default_sample_duration,
        default_sample_size,
        default_sample_flags,
    ))
}

fn parse_trak(
    trak: &[u8],
    trex_defaults: &HashMap<u32, (u32, u32, u32)>,
    movie_timescale: u32,
) -> Result<InitTrack> {
    let tkhd = find_box(trak, b"tkhd")?
        .ok_or_else(|| Error::bitstream("trak does not contain a tkhd box"))?;
    let track_id = parse_tkhd_track_id(tkhd)?;

    let mdia = find_box(trak, b"mdia")?
        .ok_or_else(|| Error::bitstream("trak does not contain an mdia box"))?;

    let mdhd = find_box(mdia, b"mdhd")?
        .ok_or_else(|| Error::bitstream("mdia does not contain an mdhd box"))?;
    let timescale = parse_mdhd_timescale(mdhd)?;

    let hdlr = find_box(mdia, b"hdlr")?
        .ok_or_else(|| Error::bitstream("mdia does not contain an hdlr box"))?;
    if hdlr.len() < 12 {
        return Err(Error::bitstream("hdlr box is too short"));
    }
    let handler_type = &hdlr[8..12];

    let minf = find_box(mdia, b"minf")?
        .ok_or_else(|| Error::bitstream("mdia does not contain a minf box"))?;
    let stbl = find_box(minf, b"stbl")?
        .ok_or_else(|| Error::bitstream("minf does not contain an stbl box"))?;
    let stsd = find_box(stbl, b"stsd")?
        .ok_or_else(|| Error::bitstream("stbl does not contain an stsd box"))?;

    let entry = parse_stsd_entry(stsd)?;

    let kind = if handler_type == b"vide" {
        entry.kind
    } else if handler_type == b"soun" {
        StreamKind::Aac
    } else {
        return Err(Error::unsupported(format!(
            "unsupported handler type: {:?}",
            handler_type
        )));
    };

    let (default_sample_duration, default_sample_size, default_sample_flags) =
        trex_defaults.get(&track_id).copied().unwrap_or((0, 0, 0));

    Ok(InitTrack {
        track_id,
        kind,
        timescale,
        timeline_offset: parse_edit_offset(trak, timescale, movie_timescale)?,
        sps: entry.sps,
        pps: entry.pps,
        vps: entry.vps,
        width: entry.width,
        height: entry.height,
        audio_specific_config: entry.audio_specific_config,
        sample_rate: entry.sample_rate,
        channel_count: entry.channel_count,
        default_sample_duration,
        default_sample_size,
        default_sample_flags,
        length_size: entry.length_size,
        #[cfg(not(target_arch = "wasm32"))]
        video_codec: entry.video_codec,
    })
}

fn parse_edit_offset(trak: &[u8], timescale: u32, movie_timescale: u32) -> Result<i128> {
    let Some(edts) = find_box(trak, b"edts")? else {
        return Ok(0);
    };
    let elst = find_box(edts, b"elst")?.ok_or_else(|| Error::bitstream("missing elst"))?;
    if elst.len() < 8 || elst[0] > 1 {
        return Err(Error::bitstream("invalid edit list"));
    }
    let count = u32::from_be_bytes(elst[4..8].try_into().unwrap());
    if count == 0 {
        return Ok(0);
    }
    if count > 2 {
        return Err(Error::unsupported("complex edit lists"));
    }
    let width = if elst[0] == 0 { 12 } else { 20 };
    if elst.len() != 8 + count as usize * width {
        return Err(Error::bitstream("truncated edit list"));
    }
    let mut empty = 0u64;
    for (index, entry) in elst[8..].chunks_exact(width).enumerate() {
        let (duration, media_time, rate) = if elst[0] == 0 {
            (
                u64::from(u32::from_be_bytes(entry[..4].try_into().unwrap())),
                i64::from(i32::from_be_bytes(entry[4..8].try_into().unwrap())),
                &entry[8..],
            )
        } else {
            (
                u64::from_be_bytes(entry[..8].try_into().unwrap()),
                i64::from_be_bytes(entry[8..16].try_into().unwrap()),
                &entry[16..],
            )
        };
        if rate != [0, 1, 0, 0] {
            return Err(Error::unsupported("non-unit edit list rate"));
        }
        if media_time == -1 && index == 0 && count == 2 {
            empty = duration;
        } else if media_time >= 0 && index + 1 == count as usize {
            let offset = i128::from(empty) * i128::from(timescale) / i128::from(movie_timescale)
                - i128::from(media_time);
            i64::try_from(offset).map_err(|_| Error::bitstream("edit timeline exceeds i64"))?;
            return Ok(offset);
        } else {
            return Err(Error::unsupported("unsupported edit list layout"));
        }
    }
    Err(Error::unsupported("edit list contains no media edit"))
}

fn parse_tkhd_track_id(tkhd: &[u8]) -> Result<u32> {
    if tkhd.len() < 4 {
        return Err(Error::bitstream("tkhd box is too short"));
    }
    let version = tkhd[0];
    let pos = if version == 0 { 4 + 8 } else { 4 + 16 };
    if tkhd.len() < pos + 4 {
        return Err(Error::bitstream("tkhd box is too short for track_id"));
    }
    Ok(u32::from_be_bytes([
        tkhd[pos],
        tkhd[pos + 1],
        tkhd[pos + 2],
        tkhd[pos + 3],
    ]))
}

fn parse_mdhd_timescale(mdhd: &[u8]) -> Result<u32> {
    if mdhd.len() < 4 {
        return Err(Error::bitstream("mdhd box is too short"));
    }
    let version = mdhd[0];
    let pos = if version == 0 { 4 + 8 } else { 4 + 16 };
    if mdhd.len() < pos + 8 {
        return Err(Error::bitstream("mdhd box is too short for timescale"));
    }
    let scale = u32::from_be_bytes(mdhd[pos..pos + 4].try_into().unwrap());
    if scale == 0 {
        return Err(Error::bitstream("zero track timescale"));
    }
    Ok(scale)
}

struct SampleEntryInfo {
    kind: StreamKind,
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
    vps: Option<Vec<u8>>,
    width: Option<u16>,
    height: Option<u16>,
    audio_specific_config: Option<Vec<u8>>,
    sample_rate: Option<u32>,
    channel_count: Option<u8>,
    length_size: usize,
    #[cfg(not(target_arch = "wasm32"))]
    video_codec: Option<crate::mp4::VideoCodec>,
}

fn parse_stsd_entry(stsd: &[u8]) -> Result<SampleEntryInfo> {
    // full box: version(1) + flags(3) + entry_count(4)
    if stsd.len() < 8 {
        return Err(Error::bitstream("stsd box is too short"));
    }
    if u32::from_be_bytes(stsd[4..8].try_into().unwrap()) != 1 {
        return Err(Error::unsupported("multiple sample descriptions"));
    }
    let entries_data = &stsd[8..];
    if entries_data.len() < 8 {
        return Err(Error::bitstream("stsd box has no entries"));
    }
    let header = read_box_header(entries_data)?;
    if header.total_size > entries_data.len() {
        return Err(Error::bitstream("stsd entry extends past data"));
    }
    let entry_payload = &entries_data[header.header_size..header.total_size];
    let entry_type = &header.box_type;

    match entry_type {
        b"avc1" | b"avc3" => {
            // VisualSampleEntry: 6 reserved + 2 dref_idx + 70 video fields = 78
            // bytes of fixed layout before the codec sub-boxes (avcC, etc.).
            // find_box cannot scan from offset 0 because those 78 bytes are
            // not box-structured.
            if entry_payload.len() < 78 {
                return Err(Error::bitstream("avc1 entry is too short"));
            }
            let avcc_data = find_box(&entry_payload[78..], b"avcC")?
                .ok_or_else(|| Error::bitstream("avc1 entry missing avcC box"))?;
            let config = avc::parse_avcc(avcc_data)?;
            let (width, height) = read_video_dimensions(entry_payload)?;
            Ok(SampleEntryInfo {
                kind: StreamKind::Avc,
                sps: Some(config.sps),
                pps: Some(config.pps),
                vps: None,
                width,
                height,
                audio_specific_config: None,
                sample_rate: None,
                channel_count: None,
                length_size: config.length_size_minus_one as usize + 1,
                #[cfg(not(target_arch = "wasm32"))]
                video_codec: Some(crate::mp4::VideoCodec::Avc {
                    avcc: avcc_data.to_vec(),
                }),
            })
        }
        b"hvc1" | b"hev1" => {
            if entry_payload.len() < 78 {
                return Err(Error::bitstream("hvc1 entry is too short"));
            }
            let hvcc_data = find_box(&entry_payload[78..], b"hvcC")?
                .ok_or_else(|| Error::bitstream("hvc1 entry missing hvcC box"))?;
            let config = hevc::parse_hvcc(hvcc_data)?;
            let (width, height) = read_video_dimensions(entry_payload)?;
            Ok(SampleEntryInfo {
                kind: StreamKind::Hevc,
                sps: Some(config.sps),
                pps: Some(config.pps),
                vps: Some(config.vps),
                width,
                height,
                audio_specific_config: None,
                sample_rate: None,
                channel_count: None,
                length_size: config.length_size_minus_one as usize + 1,
                #[cfg(not(target_arch = "wasm32"))]
                video_codec: Some(crate::mp4::VideoCodec::Hevc {
                    hvcc: hvcc_data.to_vec(),
                }),
            })
        }
        b"mp4a" => {
            // AudioSampleEntry: 6 reserved + 2 dref_idx + 20 audio fields = 28
            // bytes of fixed layout before the codec sub-boxes (esds, etc.).
            if entry_payload.len() < 28 {
                return Err(Error::bitstream("mp4a entry is too short"));
            }
            let esds_data = find_box(&entry_payload[28..], b"esds")?
                .ok_or_else(|| Error::bitstream("mp4a entry missing esds box"))?;
            let asc = parse_esds_audio_specific_config(esds_data)?;
            let channel_count = u16::from_be_bytes([entry_payload[16], entry_payload[17]]) as u8;
            let sample_rate = u16::from_be_bytes([entry_payload[24], entry_payload[25]]) as u32;
            Ok(SampleEntryInfo {
                kind: StreamKind::Aac,
                sps: None,
                pps: None,
                vps: None,
                width: None,
                height: None,
                audio_specific_config: Some(asc),
                sample_rate: Some(sample_rate),
                channel_count: Some(channel_count),
                length_size: 0,
                #[cfg(not(target_arch = "wasm32"))]
                video_codec: None,
            })
        }
        _ => Err(Error::unsupported(format!(
            "unsupported sample entry type: {:?}",
            entry_type
        ))),
    }
}

fn read_video_dimensions(entry_payload: &[u8]) -> Result<(Option<u16>, Option<u16>)> {
    if entry_payload.len() < 28 {
        return Ok((None, None));
    }
    let width = u16::from_be_bytes([entry_payload[24], entry_payload[25]]);
    let height = u16::from_be_bytes([entry_payload[26], entry_payload[27]]);
    Ok((Some(width), Some(height)))
}

// === ESDS parsing (AudioSpecificConfig extraction) ===

fn parse_esds_audio_specific_config(esds_payload: &[u8]) -> Result<Vec<u8>> {
    if esds_payload.len() < 5 {
        return Err(Error::bitstream("esds box is too short"));
    }
    let mut pos = 4; // skip version(1) + flags(3)

    // ES_Descriptor (tag 0x03)
    if pos >= esds_payload.len() || esds_payload[pos] != 0x03 {
        return Err(Error::bitstream("expected ES_Descriptor tag 0x03"));
    }
    pos += 1;
    let _es_len = read_esds_length(esds_payload, &mut pos)?;
    if pos + 3 > esds_payload.len() {
        return Err(Error::bitstream("truncated ES_Descriptor header"));
    }
    let es_flags = esds_payload[pos + 2];
    pos += 3; // ES_ID(2) + flags(1)

    if es_flags & 0x80 != 0 {
        pos += 2; // dependsOnES_ID
    }
    if es_flags & 0x40 != 0 {
        if pos >= esds_payload.len() {
            return Err(Error::bitstream("truncated ES URL"));
        }
        let url_len = esds_payload[pos] as usize;
        pos += 1 + url_len;
    }
    if es_flags & 0x20 != 0 {
        pos += 2; // OCR_ES_Id
    }

    // DecoderConfigDescriptor (tag 0x04)
    if pos >= esds_payload.len() || esds_payload[pos] != 0x04 {
        return Err(Error::bitstream(
            "expected DecoderConfigDescriptor tag 0x04",
        ));
    }
    pos += 1;
    let _dc_len = read_esds_length(esds_payload, &mut pos)?;
    if pos + 13 > esds_payload.len() {
        return Err(Error::bitstream("truncated DecoderConfigDescriptor"));
    }
    pos += 13; // objectTypeIndication(1) + streamType(1) + bufferSizeDB(3) + maxBitrate(4) + avgBitrate(4)

    // DecoderSpecificInfo (tag 0x05)
    if pos >= esds_payload.len() || esds_payload[pos] != 0x05 {
        return Err(Error::bitstream("expected DecoderSpecificInfo tag 0x05"));
    }
    pos += 1;
    let asc_len = read_esds_length(esds_payload, &mut pos)?;
    if pos + asc_len > esds_payload.len() {
        return Err(Error::bitstream("truncated AudioSpecificConfig"));
    }
    Ok(esds_payload[pos..pos + asc_len].to_vec())
}

fn read_esds_length(data: &[u8], pos: &mut usize) -> Result<usize> {
    let mut length = 0usize;
    for _ in 0..4 {
        if *pos >= data.len() {
            return Err(Error::bitstream("truncated ESDS length"));
        }
        let byte = data[*pos];
        *pos += 1;
        length = (length << 7) | (byte & 0x7f) as usize;
        if byte & 0x80 == 0 {
            return Ok(length);
        }
    }
    Err(Error::bitstream("ESDS length is too large"))
}

// === Media segment parsing ===

struct Tfhd {
    track_id: u32,
    base_data_offset: Option<u64>,
    default_base_is_moof: bool,
    default_sample_duration: Option<u32>,
    default_sample_size: Option<u32>,
    default_sample_flags: Option<u32>,
}

struct TrunSample {
    duration: u32,
    size: u32,
    flags: u32,
    composition_offset: Option<i64>,
}

struct Trun {
    data_offset: Option<i32>,
    samples: Vec<TrunSample>,
}

pub(crate) fn demux_isobmff(init: &[u8], segment: &[u8]) -> Result<DemuxOutput> {
    demux_isobmff_checked(init, segment, &|| Ok(()))
}

pub(crate) fn demux_isobmff_checked(
    init: &[u8],
    segment: &[u8],
    check: &dyn Fn() -> Result<()>,
) -> Result<DemuxOutput> {
    check()?;
    let tracks = parse_init_segment(init)?;
    let mut output = DemuxOutput::default();

    for track in &tracks {
        match track.kind {
            StreamKind::Avc | StreamKind::Hevc => {
                if output.vps.is_none() {
                    output.vps = track.vps.clone();
                }
                if output.sps.is_none() {
                    output.sps = track.sps.clone();
                }
                if output.pps.is_none() {
                    output.pps = track.pps.clone();
                }
                if output.width.is_none() {
                    output.width = track.width;
                }
                if output.height.is_none() {
                    output.height = track.height;
                }
            }
            StreamKind::Aac => {
                output.audio_specific_config = track.audio_specific_config.clone();
                output.sample_rate = track.sample_rate;
                output.channel_count = track.channel_count;
            }
        }
    }

    parse_media_segment(segment, &tracks, &mut output, check)?;
    Ok(output)
}

fn parse_media_segment(
    segment: &[u8],
    tracks: &[InitTrack],
    output: &mut DemuxOutput,
    check: &dyn Fn() -> Result<()>,
) -> Result<()> {
    let mut offset = 0;
    while offset + 8 <= segment.len() {
        check()?;
        let header = read_box_header(&segment[offset..])?;
        if offset
            .checked_add(header.total_size)
            .is_none_or(|end| end > segment.len())
        {
            return Err(Error::bitstream("ISOBMFF box extends past segment"));
        }
        if &header.box_type == b"moof" {
            let moof_payload = &segment[offset + header.header_size..offset + header.total_size];
            parse_moof(moof_payload, offset, segment, tracks, output, check)?;
        }
        offset += header.total_size;
    }
    Ok(())
}

fn parse_moof(
    moof_payload: &[u8],
    moof_offset: usize,
    segment: &[u8],
    tracks: &[InitTrack],
    output: &mut DemuxOutput,
    check: &dyn Fn() -> Result<()>,
) -> Result<()> {
    // A moof may contain multiple traf boxes (one per track). Iterate all
    // top-level children of moof and process each traf. Using find_box here
    // would only return the first traf and silently drop the other tracks.
    let mut offset = 0;
    let mut saw_traf = false;
    while offset + 8 <= moof_payload.len() {
        check()?;
        let header = read_box_header(&moof_payload[offset..])?;
        if offset
            .checked_add(header.total_size)
            .is_none_or(|end| end > moof_payload.len())
        {
            return Err(Error::bitstream("ISOBMFF box extends past moof"));
        }
        if &header.box_type == b"traf" {
            saw_traf = true;
            let traf_payload =
                &moof_payload[offset + header.header_size..offset + header.total_size];
            parse_traf(traf_payload, moof_offset, segment, tracks, output, check)?;
        }
        offset += header.total_size;
    }
    if !saw_traf {
        return Err(Error::bitstream("moof does not contain a traf box"));
    }
    Ok(())
}

fn parse_traf(
    traf: &[u8],
    moof_offset: usize,
    segment: &[u8],
    tracks: &[InitTrack],
    output: &mut DemuxOutput,
    check: &dyn Fn() -> Result<()>,
) -> Result<()> {
    let tfhd_data = find_box(traf, b"tfhd")?
        .ok_or_else(|| Error::bitstream("traf does not contain a tfhd box"))?;
    let tfhd = parse_tfhd(tfhd_data)?;

    let track = tracks
        .iter()
        .find(|t| t.track_id == tfhd.track_id)
        .ok_or_else(|| Error::bitstream("moof references unknown track_id"))?;

    let base_data_offset = tfhd.base_data_offset.unwrap_or(moof_offset as u64);

    let base_decode_time = if let Some(tfdt_data) = find_box(traf, b"tfdt")? {
        parse_tfdt(tfdt_data)?
    } else {
        0
    };

    let mut mdats = Vec::new();
    let mut box_offset = 0;
    while box_offset < segment.len() {
        let header = read_box_header(&segment[box_offset..])?;
        let end = box_offset
            .checked_add(header.total_size)
            .filter(|&n| n <= segment.len())
            .ok_or_else(|| Error::bitstream("box extends past segment"))?;
        if header.box_type == *b"mdat" {
            mdats.push(((box_offset + header.header_size) as u64, end as u64));
        }
        box_offset = end;
    }
    if tfhd.base_data_offset.is_none() && !tfhd.default_base_is_moof {
        return Err(Error::unsupported(
            "implicit cross-traf base data offsets are unsupported",
        ));
    }
    let mut cumulative_duration = 0u64;
    let mut sample_data_offset = base_data_offset;
    let mut runs = 0;
    for_each_box(traf, |kind, data| {
        if kind != b"trun" {
            return Ok(());
        }
        let trun = parse_trun(data, &tfhd, track)?;
        if let Some(offset) = trun.data_offset {
            sample_data_offset = base_data_offset
                .checked_add_signed(i64::from(offset))
                .ok_or_else(|| Error::bitstream("sample offset overflow"))?;
        } else if runs == 0 {
            sample_data_offset = base_data_offset;
        }
        runs += 1;
        for sample in &trun.samples {
            check()?;
            let end = sample_data_offset
                .checked_add(u64::from(sample.size))
                .filter(|&end| {
                    mdats
                        .iter()
                        .any(|&(start, limit)| sample_data_offset >= start && end <= limit)
                })
                .ok_or_else(|| Error::bitstream("sample data extends past segment"))?;
            let raw = &segment[sample_data_offset as usize..end as usize];
            let is_key = is_key_sample(sample.flags, raw, track);
            let data = if matches!(track.kind, StreamKind::Avc | StreamKind::Hevc) {
                normalize_nals(raw, track)?
            } else {
                raw.to_vec()
            };

            let dts = base_decode_time
                .checked_add(cumulative_duration)
                .ok_or_else(|| Error::bitstream("decode timestamp overflow"))?;
            let pts = i128::from(dts) + i128::from(sample.composition_offset.unwrap_or(0));

            let dts_90k = rescale_to_90k(dts, track.timescale)?;
            let pts_90k = rescale_to_90k(u64::try_from(pts).unwrap_or(0), track.timescale)?;

            match track.kind {
                StreamKind::Avc | StreamKind::Hevc => {
                    output.saw_video = true;
                    output.video_timescale = Some(track.timescale);
                }
                StreamKind::Aac => {
                    output.saw_audio = true;
                    output.audio_timescale = Some(track.timescale);
                }
            }

            let duration = u64::from(sample.duration);

            output.packets.push(EncodedPacket {
                kind: track.kind,
                timing: Some(crate::types::PacketTiming {
                    edit_offset: track.timeline_offset,
                    timescale: track.timescale,
                    dts: i128::from(dts) + track.timeline_offset,
                    pts: pts + track.timeline_offset,
                    duration: sample.duration,
                }),
                data,
                pts_90k: i128::from(pts_90k),
                dts_90k,
                duration,
                is_key,
                is_length_prefixed: matches!(track.kind, StreamKind::Avc | StreamKind::Hevc),
            });

            if sample.duration == 0 {
                return Err(Error::bitstream("zero fMP4 sample duration"));
            }
            cumulative_duration = cumulative_duration
                .checked_add(u64::from(sample.duration))
                .ok_or_else(|| Error::bitstream("duration overflow"))?;
            sample_data_offset = end;
        }

        Ok(())
    })?;
    if runs == 0 {
        return Err(Error::bitstream("traf does not contain a trun"));
    }

    Ok(())
}

fn parse_tfhd(data: &[u8]) -> Result<Tfhd> {
    if data.len() < 8 {
        return Err(Error::bitstream("tfhd box is too short"));
    }
    let flags = u32::from_be_bytes([0, data[1], data[2], data[3]]);
    let track_id = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);

    let mut pos = 8;
    let mut base_data_offset = None;
    let mut default_sample_duration = None;
    let mut default_sample_size = None;
    let mut default_sample_flags = None;

    if flags & 0x000001 != 0 {
        if pos + 8 > data.len() {
            return Err(Error::bitstream("truncated tfhd base_data_offset"));
        }
        base_data_offset = Some(u64::from_be_bytes([
            data[pos],
            data[pos + 1],
            data[pos + 2],
            data[pos + 3],
            data[pos + 4],
            data[pos + 5],
            data[pos + 6],
            data[pos + 7],
        ]));
        pos += 8;
    }
    if flags & 0x000002 != 0 {
        if pos + 4 > data.len() || u32::from_be_bytes(data[pos..pos + 4].try_into().unwrap()) != 1 {
            return Err(Error::unsupported("unsupported sample description index"));
        }
        pos += 4;
    }
    if flags & 0x000008 != 0 {
        if pos + 4 > data.len() {
            return Err(Error::bitstream("truncated tfhd default_sample_duration"));
        }
        default_sample_duration = Some(u32::from_be_bytes([
            data[pos],
            data[pos + 1],
            data[pos + 2],
            data[pos + 3],
        ]));
        pos += 4;
    }
    if flags & 0x000010 != 0 {
        if pos + 4 > data.len() {
            return Err(Error::bitstream("truncated tfhd default_sample_size"));
        }
        default_sample_size = Some(u32::from_be_bytes([
            data[pos],
            data[pos + 1],
            data[pos + 2],
            data[pos + 3],
        ]));
        pos += 4;
    }
    if flags & 0x000020 != 0 {
        if pos + 4 > data.len() {
            return Err(Error::bitstream("truncated tfhd default_sample_flags"));
        }
        default_sample_flags = Some(u32::from_be_bytes([
            data[pos],
            data[pos + 1],
            data[pos + 2],
            data[pos + 3],
        ]));
    }

    Ok(Tfhd {
        track_id,
        base_data_offset,
        default_base_is_moof: flags & 0x020000 != 0,
        default_sample_duration,
        default_sample_size,
        default_sample_flags,
    })
}

fn parse_tfdt(data: &[u8]) -> Result<u64> {
    if data.len() < 4 {
        return Err(Error::bitstream("tfdt box is too short"));
    }
    let version = data[0];
    if version == 0 {
        if data.len() < 4 + 4 {
            return Err(Error::bitstream("tfdt v0 is too short"));
        }
        Ok(u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as u64)
    } else {
        if data.len() < 4 + 8 {
            return Err(Error::bitstream("tfdt v1 is too short"));
        }
        Ok(u64::from_be_bytes([
            data[4], data[5], data[6], data[7], data[8], data[9], data[10], data[11],
        ]))
    }
}

fn parse_trun(data: &[u8], tfhd: &Tfhd, track: &InitTrack) -> Result<Trun> {
    if data.len() < 8 {
        return Err(Error::bitstream("trun box is too short"));
    }
    if data[0] > 1 {
        return Err(Error::bitstream("unsupported trun version"));
    }
    let flags = u32::from_be_bytes([0, data[1], data[2], data[3]]);
    let sample_count = u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as usize;

    let mut pos = 8;

    let data_offset = if flags & 0x000001 != 0 {
        if pos + 4 > data.len() {
            return Err(Error::bitstream("truncated trun data_offset"));
        }
        let off = i32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
        pos += 4;
        Some(off)
    } else {
        None
    };

    let first_flags = if flags & 0x000004 != 0 {
        if pos + 4 > data.len() || flags & 0x000400 != 0 {
            return Err(Error::bitstream("invalid first_sample_flags"));
        }
        let value = u32::from_be_bytes(data[pos..pos + 4].try_into().unwrap());
        pos += 4;
        Some(value)
    } else {
        None
    };

    let has_duration = flags & 0x000100 != 0;
    let has_size = flags & 0x000200 != 0;
    let has_flags = flags & 0x000400 != 0;
    let has_cts = flags & 0x000800 != 0;

    let default_duration = tfhd
        .default_sample_duration
        .unwrap_or(track.default_sample_duration);
    let default_size = tfhd
        .default_sample_size
        .unwrap_or(track.default_sample_size);
    let default_flags = tfhd
        .default_sample_flags
        .unwrap_or(track.default_sample_flags);

    let fields =
        u32::from(has_duration) + u32::from(has_size) + u32::from(has_flags) + u32::from(has_cts);
    let needed = sample_count
        .checked_mul(fields as usize * 4)
        .ok_or_else(|| Error::bitstream("trun count overflow"))?;
    if pos > data.len() || needed > data.len() - pos || sample_count > 16_777_216 {
        return Err(Error::bitstream("invalid trun sample count"));
    }
    let mut samples = Vec::new();
    samples
        .try_reserve_exact(sample_count)
        .map_err(|_| Error::bitstream("trun allocation failed"))?;
    for index in 0..sample_count {
        let duration = if has_duration {
            if pos + 4 > data.len() {
                return Err(Error::bitstream("truncated trun sample duration"));
            }
            let d = u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
            pos += 4;
            d
        } else {
            default_duration
        };

        let size = if has_size {
            if pos + 4 > data.len() {
                return Err(Error::bitstream("truncated trun sample size"));
            }
            let s = u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
            pos += 4;
            s
        } else {
            default_size
        };

        let sample_flags = if has_flags {
            if pos + 4 > data.len() {
                return Err(Error::bitstream("truncated trun sample flags"));
            }
            let f = u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
            pos += 4;
            f
        } else {
            if index == 0 {
                first_flags.unwrap_or(default_flags)
            } else {
                default_flags
            }
        };

        let composition_offset = if has_cts {
            if pos + 4 > data.len() {
                return Err(Error::bitstream("truncated trun sample composition offset"));
            }
            let raw = [data[pos], data[pos + 1], data[pos + 2], data[pos + 3]];
            pos += 4;
            Some(if data[0] == 0 {
                i64::from(u32::from_be_bytes(raw))
            } else if data[0] == 1 {
                i64::from(i32::from_be_bytes(raw))
            } else {
                return Err(Error::bitstream("unsupported trun version"));
            })
        } else {
            None
        };

        samples.push(TrunSample {
            duration,
            size,
            flags: sample_flags,
            composition_offset,
        });
    }

    Ok(Trun {
        data_offset,
        samples,
    })
}

fn normalize_nals(data: &[u8], track: &InitTrack) -> Result<Vec<u8>> {
    if !matches!(track.length_size, 1 | 2 | 4) {
        return Err(Error::unsupported("unsupported NAL length prefix width"));
    }
    let mut output = Vec::new();
    let mut offset = 0usize;
    while offset < data.len() {
        if data.len() - offset < track.length_size {
            return Err(Error::bitstream("truncated NAL prefix"));
        }
        let length = read_nal_length(data, offset, track.length_size);
        offset += track.length_size;
        let end = offset
            .checked_add(length)
            .filter(|&end| end <= data.len())
            .ok_or_else(|| Error::bitstream("truncated NAL payload"))?;
        if length == 0 || matches!(track.kind, StreamKind::Hevc) && length < 2 {
            return Err(Error::bitstream("invalid NAL size"));
        }
        let nal = &data[offset..end];
        let expected = match track.kind {
            StreamKind::Avc => match nal[0] & 0x1f {
                7 => track.sps.as_deref(),
                8 => track.pps.as_deref(),
                _ => None,
            },
            StreamKind::Hevc => match (nal[0] >> 1) & 0x3f {
                32 => track.vps.as_deref(),
                33 => track.sps.as_deref(),
                34 => track.pps.as_deref(),
                _ => None,
            },
            StreamKind::Aac => None,
        };
        if expected.is_some_and(|parameter| parameter != nal) {
            return Err(Error::unsupported("in-band codec parameter set changes"));
        }
        output.extend_from_slice(&(length as u32).to_be_bytes());
        output.extend_from_slice(nal);
        offset = end;
    }
    Ok(output)
}

// === Keyframe detection ===

fn is_key_sample(flags: u32, data: &[u8], track: &InitTrack) -> bool {
    // ISOBMFF sample_flags bit layout: sample_depends_on occupies bits 25-24.
    //   0 = unknown           → fall back to NAL probing
    //   1 = depends on others → P/B frame (not a sync sample)
    //   2 = independent       → I frame (sync sample)
    //   3 = reserved
    let sample_depends_on = (flags >> 24) & 0x3;
    if sample_depends_on == 2 {
        return true;
    }
    if sample_depends_on == 1 {
        return false;
    }
    // sample_depends_on == 0 (unknown): fall back to NAL probing
    match track.kind {
        StreamKind::Avc => probe_avc_keyframe(data, track.length_size),
        StreamKind::Hevc => probe_hevc_keyframe(data, track.length_size),
        StreamKind::Aac => true,
    }
}

fn probe_avc_keyframe(data: &[u8], length_size: usize) -> bool {
    let mut offset = 0;
    while offset + length_size <= data.len() {
        let len = read_nal_length(data, offset, length_size);
        if len == 0 {
            break;
        }
        offset += length_size;
        if offset + 1 > data.len() {
            break;
        }
        let nal_type = data[offset] & 0x1f;
        if nal_type == 5 {
            return true;
        }
        offset += len;
    }
    false
}

fn probe_hevc_keyframe(data: &[u8], length_size: usize) -> bool {
    let mut offset = 0;
    while offset + length_size <= data.len() {
        let len = read_nal_length(data, offset, length_size);
        if len < 2 {
            break;
        }
        offset += length_size;
        if offset + 2 > data.len() {
            break;
        }
        let nal_type = (data[offset] >> 1) & 0x3f;
        if (16..=23).contains(&nal_type) {
            return true;
        }
        offset += len;
    }
    false
}

fn read_nal_length(data: &[u8], offset: usize, length_size: usize) -> usize {
    match length_size {
        1 => data[offset] as usize,
        2 => u16::from_be_bytes([data[offset], data[offset + 1]]) as usize,
        4 => u32::from_be_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]) as usize,
        _ => 0,
    }
}

fn rescale_to_90k(value: u64, timescale: u32) -> Result<u64> {
    if timescale == 0 {
        return Err(Error::bitstream("zero timescale"));
    }
    u64::try_from(u128::from(value) * 90_000 / u128::from(timescale))
        .map_err(|_| Error::bitstream("timestamp exceeds checkpoint time domain"))
}

/// Strict resource framing for the new AES path only. Existing demux behavior is unchanged.
/// Sample offsets/codecs are validated by demux after a matching MAP is available.
pub(crate) fn validate_resource_envelope(data: &[u8], init: bool) -> Result<()> {
    let (mut ftyp, mut moov, mut moof, mut mdat) = (0, 0, 0, 0);
    let mut awaiting_data = false;
    for_each_box(data, |kind, payload| {
        match kind {
            b"ftyp" => {
                ftyp += 1;
                if payload.len() < 8 || payload.len() % 4 != 0 {
                    return Err(Error::bitstream("invalid ftyp"));
                }
            }
            b"moov" => {
                moov += 1;
                if !init {
                    return Err(Error::unsupported(
                        "media resource contains a new initialization",
                    ));
                }
                for_each_box(payload, |_, _| Ok(()))?;
                if find_box(payload, b"mvex")?.is_none() {
                    return Err(Error::bitstream("fMP4 initialization missing mvex"));
                }
            }
            b"moof" => {
                if init || awaiting_data {
                    return Err(Error::bitstream("unexpected moof"));
                }
                moof += 1;
                awaiting_data = true;
                let (mut headers, mut tracks) = (0, 0);
                for_each_box(payload, |kind, payload| {
                    match kind {
                        b"mfhd" => {
                            headers += 1;
                            if payload.len() != 8 || payload[..4] != [0; 4] {
                                return Err(Error::bitstream("invalid mfhd"));
                            }
                        }
                        b"traf" => {
                            tracks += 1;
                            let (mut tfhd, mut runs) = (0, 0);
                            for_each_box(payload, |kind, payload| {
                                match kind {
                                    b"tfhd" => {
                                        tfhd += 1;
                                        parse_tfhd(payload)?;
                                    }
                                    b"tfdt" => {
                                        parse_tfdt(payload)?;
                                    }
                                    b"trun" => {
                                        runs += 1;
                                        if payload.len() < 8 {
                                            return Err(Error::bitstream("truncated trun"));
                                        }
                                    }
                                    b"senc" | b"saiz" | b"saio" | b"sgpd" | b"sbgp" => {
                                        return Err(Error::unsupported(
                                            "sample protection/group metadata is not supported in resource-only profile",
                                        ));
                                    }
                                    _ => {}
                                }
                                Ok(())
                            })?;
                            if tfhd != 1 || runs == 0 {
                                return Err(Error::bitstream("traf missing unique tfhd or runs"));
                            }
                        }
                        _ => {}
                    }
                    Ok(())
                })?;
                if headers != 1 || tracks == 0 {
                    return Err(Error::bitstream("moof missing unique mfhd or tracks"));
                }
            }
            b"mdat" => {
                if init || !awaiting_data || payload.is_empty() {
                    return Err(Error::bitstream("unexpected or empty mdat"));
                }
                mdat += 1;
                awaiting_data = false;
            }
            _ => {}
        }
        Ok(())
    })?;
    if init {
        if ftyp != 1 || moov != 1 {
            return Err(Error::bitstream("initialization missing unique ftyp/moov"));
        }
        parse_init_segment(data)?;
    } else if moof == 0 || moof != mdat || awaiting_data {
        return Err(Error::bitstream("incomplete fMP4 resource"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_box_in_data() {
        let data = boxed(b"ftyp", |o| o.extend_from_slice(b"isom"));
        let found = find_box(&data, b"ftyp").unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap(), b"isom");
    }

    #[test]
    fn returns_none_for_missing_box() {
        let data = boxed(b"ftyp", |o| o.extend_from_slice(b"isom"));
        let found = find_box(&data, b"moov").unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn rescales_to_90k() {
        assert_eq!(rescale_to_90k(44100, 44100).unwrap(), 90000);
        assert_eq!(rescale_to_90k(12800, 12800).unwrap(), 90000);
        assert_eq!(rescale_to_90k(100, 90000).unwrap(), 100);
    }

    fn audio_init() -> Vec<u8> {
        crate::mp4::FragmentedMp4Muxer::new(vec![crate::mp4::FragmentedTrack::audio(
            1,
            48000,
            2,
            vec![0x11, 0x90],
        )])
        .write_header()
        .unwrap()
    }

    #[test]
    fn multiple_runs_preserve_offsets_defaults_and_native_timing() {
        let init = audio_init();
        let segment = boxed(b"mdat", |o| o.extend_from_slice(&[1, 2, 3, 4]));
        let mut traf = boxed(b"tfhd", |o| {
            o.extend_from_slice(&[0, 2, 0, 0]);
            o.extend_from_slice(&1u32.to_be_bytes());
        });
        traf.extend(boxed(b"tfdt", |o| {
            o.extend_from_slice(&[1, 0, 0, 0]);
            o.extend_from_slice(&123u64.to_be_bytes());
        }));
        for (flags, offset, cts) in [(0xf01u32, Some(8i32), -4i32), (0xf00, None, 2)] {
            traf.extend(boxed(b"trun", |o| {
                o.push(1);
                o.extend_from_slice(&flags.to_be_bytes()[1..]);
                o.extend_from_slice(&1u32.to_be_bytes());
                if let Some(offset) = offset {
                    o.extend_from_slice(&offset.to_be_bytes());
                }
                for n in [1000u32, 2, 0x02000000, cts as u32] {
                    o.extend_from_slice(&n.to_be_bytes());
                }
            }));
        }
        let mut output = DemuxOutput::default();
        parse_traf(
            &traf,
            0,
            &segment,
            &parse_init_segment(&init).unwrap(),
            &mut output,
            &|| Ok(()),
        )
        .unwrap();
        assert_eq!(output.packets.len(), 2);
        assert_eq!(output.packets[0].data, [1, 2]);
        assert_eq!(output.packets[1].data, [3, 4]);
        let time = output.packets[1].timing.unwrap();
        assert_eq!(
            (time.timescale, time.dts, time.pts, time.duration),
            (48000, 1123, 1125, 1000)
        );
        let mut bad = traf.clone();
        let run = bad.windows(4).position(|v| v == b"trun").unwrap();
        bad[run + 12..run + 16].copy_from_slice(&0i32.to_be_bytes());
        assert!(
            parse_traf(
                &bad,
                0,
                &segment,
                &parse_init_segment(&init).unwrap(),
                &mut DemuxOutput::default(),
                &|| Ok(())
            )
            .is_err()
        );
    }

    #[test]
    fn trun_first_flags_and_unsigned_composition_offsets() {
        let tracks = parse_init_segment(&audio_init()).unwrap();
        let tfhd = parse_tfhd(&[0, 2, 0, 0, 0, 0, 0, 1]).unwrap();
        let mut data = vec![0, 0, 8, 4];
        for n in [1u32, 0x01010000, u32::MAX] {
            data.extend_from_slice(&n.to_be_bytes());
        }
        let run = parse_trun(&data, &tfhd, &tracks[0]).unwrap();
        assert_eq!(run.samples[0].flags, 0x01010000);
        assert_eq!(run.samples[0].composition_offset, Some(i64::from(u32::MAX)));
        data.pop();
        assert!(parse_trun(&data, &tfhd, &tracks[0]).is_err());
    }

    #[test]
    fn nal_widths_are_normalized_and_parameter_changes_rejected() {
        let demux =
            crate::mpeg_ts::demux_ts(include_bytes!("../tests/fixtures/h264_aac_fhd.ts")).unwrap();
        let init = crate::mp4::FragmentedMp4Muxer::new(vec![
            crate::mp4::FragmentedTrack::avc_video(
                1,
                demux.sps.as_ref().unwrap(),
                demux.pps.as_ref().unwrap(),
            )
            .unwrap(),
        ])
        .write_header()
        .unwrap();
        let mut tracks = parse_init_segment(&init).unwrap();
        for width in [1, 2, 4] {
            tracks[0].length_size = width;
            for kind in [StreamKind::Avc, StreamKind::Hevc] {
                tracks[0].kind = kind;
                let nal = if matches!(kind, StreamKind::Avc) {
                    vec![0x65, 1, 2]
                } else {
                    vec![0x26, 1, 2]
                };
                let mut data = (nal.len() as u32).to_be_bytes()[4 - width..].to_vec();
                data.extend(&nal);
                let normalized = normalize_nals(&data, &tracks[0]).unwrap();
                assert_eq!(&normalized[..4], &[0, 0, 0, 3]);
                assert_eq!(&normalized[4..], &nal);
                data.pop();
                assert!(normalize_nals(&data, &tracks[0]).is_err());
            }
        }
        tracks[0].kind = StreamKind::Avc;
        tracks[0].length_size = 1;
        assert!(matches!(
            normalize_nals(&[2, 0x67, 0], &tracks[0]),
            Err(Error::Unsupported(_))
        ));
        tracks[0].length_size = 3;
        assert!(normalize_nals(&[0, 0, 1, 0x65], &tracks[0]).is_err());
    }

    fn boxed(kind: &[u8; 4], write: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
        let mut payload = Vec::new();
        write(&mut payload);
        let mut out = Vec::with_capacity(8 + payload.len());
        out.extend_from_slice(&(8 + payload.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(&payload);
        out
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[path = "file_scan.rs"]
mod file_scan;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use file_scan::{FileIndex, scan as scan_file};

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn file_tracks(init: &[u8]) -> Result<Vec<crate::mp4::FragmentedTrack>> {
    use crate::mp4::{FragmentedTrack, FragmentedTrackKind};
    parse_init_segment(init)?
        .into_iter()
        .map(|t| {
            let kind = match t.kind {
                StreamKind::Avc | StreamKind::Hevc => FragmentedTrackKind::Video {
                    width: t.width.ok_or_else(|| Error::invalid("missing width"))?,
                    height: t.height.ok_or_else(|| Error::invalid("missing height"))?,
                    codec: t
                        .video_codec
                        .ok_or_else(|| Error::invalid("missing codec config"))?,
                },
                StreamKind::Aac => FragmentedTrackKind::Audio {
                    sample_rate: t
                        .sample_rate
                        .ok_or_else(|| Error::invalid("missing sample rate"))?,
                    channel_count: t
                        .channel_count
                        .ok_or_else(|| Error::invalid("missing channels"))?,
                    audio_specific_config: t
                        .audio_specific_config
                        .ok_or_else(|| Error::invalid("missing AAC config"))?,
                },
            };
            Ok(FragmentedTrack {
                track_id: t.track_id,
                timescale: t.timescale,
                kind,
            })
        })
        .collect()
}

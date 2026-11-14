use crate::codecs::avc;
#[cfg(not(target_arch = "wasm32"))]
use crate::mp4::Mp4Muxer;
use crate::mp4::{FragmentedMp4Muxer, FragmentedTrack, Mp4Sample};
use crate::types::{DemuxOutput, EncodedPacket, PacketTiming, StreamKind, TransmuxReport};
use crate::{CancelToken, CheckpointDurability, Error, Result, Source, SourceSessionOptions};
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;
use std::{cmp::Ordering, collections::VecDeque, sync::Arc};
use tokio::io::{AsyncWrite, AsyncWriteExt};
mod core;
pub use core::*;
use core::{fail, media_error};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputRole {
    Primary,
    Audio,
}
/// Native classic MP4 finalization. Does not apply to fragmented output or recovery.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum FileFinalizationStrategy {
    /// Copy samples to a compact file with a front-loaded moov (faststart).
    #[default]
    Copy,
    /// Write a faststart moov into reserved header space in the temporary file.
    /// If necessary, move media towards the end, copying from back to front.
    /// Avoids a second full file; interrupted finalization is not recoverable.
    InPlace,
}

#[derive(Debug, Clone)]
pub struct FileOutputOptions {
    format: OutputFormat,
    #[cfg(not(target_arch = "wasm32"))]
    finalization: FileFinalizationStrategy,
    #[cfg(not(target_arch = "wasm32"))]
    header_reserve: u32,
}
impl Default for FileOutputOptions {
    fn default() -> Self {
        Self {
            format: OutputFormat::StreamingMp4,
            #[cfg(not(target_arch = "wasm32"))]
            finalization: FileFinalizationStrategy::Copy,
            #[cfg(not(target_arch = "wasm32"))]
            header_reserve: 1024 * 1024,
        }
    }
}
impl FileOutputOptions {
    /// Select Native classic file finalization. Ignored for FragmentedMp4.
    /// Recoverable output uses RecoveryOptions and retains its copy strategy.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn with_finalization_strategy(mut self, strategy: FileFinalizationStrategy) -> Self {
        self.finalization = strategy;
        self
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn finalization_strategy(&self) -> FileFinalizationStrategy {
        self.finalization
    }

    /// Bytes reserved for the faststart index with InPlace (default 1 MiB).
    /// Must be zero or at least eight. Zero always uses same-file relocation.
    /// Ignored by Copy and FragmentedMp4. Insufficient space never downgrades
    /// to a tail index: media is shifted within the same file instead.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn with_header_reserve_bytes(mut self, bytes: u32) -> Self {
        self.header_reserve = bytes;
        self
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn header_reserve_bytes(&self) -> u32 {
        self.header_reserve
    }

    pub fn with_format(mut self, format: OutputFormat) -> Self {
        self.format = format;
        self
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaTime {
    ticks: i128,
    timescale: u32,
}
impl MediaTime {
    pub fn new(ticks: i128, timescale: u32) -> Result<Self> {
        if timescale == 0 {
            return Err(Error::invalid("zero timescale"));
        }
        Ok(Self { ticks, timescale })
    }
    pub fn ticks(&self) -> i128 {
        self.ticks
    }
    pub fn timescale(&self) -> u32 {
        self.timescale
    }
}
fn unwrap_near(raw: i128, reference: i128) -> Result<i128> {
    let period = 1i128 << 33;
    let delta = (raw.rem_euclid(period) - reference.rem_euclid(period)).rem_euclid(period);
    if delta == period / 2 {
        return Err(Error::unsupported("ambiguous TS wrap epoch"));
    }
    reference
        .checked_add(if delta > period / 2 {
            delta - period
        } else {
            delta
        })
        .ok_or_else(|| Error::bitstream("TS timestamp overflow"))
}
fn merge_probe_config(previous: &mut Option<DemuxOutput>, current: &mut DemuxOutput) -> Result<()> {
    if let Some(old) = previous.as_ref() {
        macro_rules! merge {
            ($($field:ident),*) => {$(
                if let (Some(a), Some(b)) = (&old.$field, &current.$field) && a != b {
                    return Err(Error::unsupported("configuration changed during probe"));
                }
                if current.$field.is_none() { current.$field = old.$field.clone(); }
            )*};
        }
        merge!(
            video_timescale,
            audio_timescale,
            vps,
            sps,
            pps,
            width,
            height,
            audio_specific_config,
            sample_rate,
            channel_count
        );
        current.saw_video |= old.saw_video;
        current.saw_audio |= old.saw_audio;
    }
    *previous = None;
    check_media_config(previous, current)
}
fn packet_time(packet: &EncodedPacket, shift: i128) -> MediaTime {
    packet.timing.map_or(
        MediaTime {
            ticks: i128::from(packet.dts_90k) + shift,
            timescale: 90_000,
        },
        |t| MediaTime {
            ticks: t.dts,
            timescale: t.timescale,
        },
    )
}
fn select_track(data: &mut DemuxOutput, role: InputRole) {
    let audio = role == InputRole::Audio;
    data.packets
        .retain(|p| matches!(p.kind, StreamKind::Aac) == audio);
    if audio {
        data.saw_video = false;
        data.video_timescale = None;
        data.vps = None;
        data.sps = None;
        data.pps = None;
        data.width = None;
        data.height = None;
    } else {
        data.saw_audio = false;
        data.audio_timescale = None;
        data.audio_specific_config = None;
        data.sample_rate = None;
        data.channel_count = None;
    }
}
struct SessionSource(Arc<dyn Source>);
impl Drop for SessionSource {
    fn drop(&mut self) {
        self.0.stop_session();
    }
}
#[cfg(not(target_arch = "wasm32"))]
struct TemporaryFile(std::path::PathBuf);
#[cfg(not(target_arch = "wasm32"))]
impl Drop for TemporaryFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
#[cfg(not(target_arch = "wasm32"))]
fn temporary_file(target: &Path) -> Result<(TemporaryFile, std::fs::File)> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    loop {
        let path = parent.join(format!(
            ".hls-transmux-session-{}-{}.mp4",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => return Ok((TemporaryFile(path), file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
}
impl crate::state_codec::StateCodec for MediaTime {
    fn put(&self, out: &mut Vec<u8>) {
        self.ticks.put(out);
        self.timescale.put(out);
    }
    fn get(r: &mut crate::state_codec::Reader<'_>) -> crate::state_codec::DecodeResult<Self> {
        Self::new(
            crate::state_codec::StateCodec::get(r)?,
            crate::state_codec::StateCodec::get(r)?,
        )
        .map_err(|_| ())
    }
}
fn config_digest(tracks: &[FragmentedTrack]) -> [u8; 32] {
    use crate::mp4::{FragmentedTrackKind, VideoCodec};
    let mut bytes = Vec::new();
    fn field(out: &mut Vec<u8>, data: &[u8]) {
        out.extend_from_slice(&(data.len() as u64).to_be_bytes());
        out.extend_from_slice(data);
    }
    bytes.extend_from_slice(b"hls-transmux-codecs-v1");
    bytes.extend_from_slice(&(tracks.len() as u64).to_be_bytes());
    for track in tracks {
        bytes.extend_from_slice(&track.track_id.to_be_bytes());
        bytes.extend_from_slice(&track.timescale.to_be_bytes());
        if let Some(meta) = &track.metadata {
            field(&mut bytes, meta.language.as_bytes());
            field(&mut bytes, meta.name.as_bytes());
            bytes.push(u8::from(meta.default));
            bytes.extend_from_slice(&meta.group.to_be_bytes());
        }
        match &track.kind {
            FragmentedTrackKind::Wvtt => field(&mut bytes, b"wvtt"),
            FragmentedTrackKind::Video {
                width,
                height,
                codec,
            } => {
                bytes.extend_from_slice(&width.to_be_bytes());
                bytes.extend_from_slice(&height.to_be_bytes());
                match codec {
                    VideoCodec::Avc { avcc } => {
                        field(&mut bytes, b"avc");
                        field(&mut bytes, avcc);
                    }
                    VideoCodec::Hevc { hvcc } => {
                        field(&mut bytes, b"hevc");
                        field(&mut bytes, hvcc);
                    }
                }
            }
            FragmentedTrackKind::Audio {
                sample_rate,
                channel_count,
                audio_specific_config,
            } => {
                field(&mut bytes, b"aac");
                bytes.extend_from_slice(&sample_rate.to_be_bytes());
                bytes.push(*channel_count);
                field(&mut bytes, audio_specific_config);
            }
        }
    }
    crate::resume::digest(&bytes)
}
fn build_fragmented_tracks(first: &DemuxOutput) -> Result<Vec<FragmentedTrack>> {
    let mut tracks = Vec::new();
    let mut next_track_id = 1_u32;

    if first.saw_video {
        if let Some(vps) = &first.vps {
            let sps = first
                .sps
                .as_ref()
                .ok_or_else(|| Error::bitstream("HEVC SPS was not found in first segment"))?;
            let pps = first
                .pps
                .as_ref()
                .ok_or_else(|| Error::bitstream("HEVC PPS was not found in first segment"))?;
            tracks.push(FragmentedTrack::hevc_video(next_track_id, vps, sps, pps)?);
        } else {
            let sps = first
                .sps
                .as_ref()
                .ok_or_else(|| Error::bitstream("H.264 SPS was not found in first segment"))?;
            let pps = first
                .pps
                .as_ref()
                .ok_or_else(|| Error::bitstream("H.264 PPS was not found in first segment"))?;
            tracks.push(FragmentedTrack::avc_video(next_track_id, sps, pps)?);
        }
        next_track_id += 1;
    }

    if first.saw_audio {
        let sample_rate = first
            .sample_rate
            .ok_or_else(|| Error::bitstream("AAC sample rate was not found in first segment"))?;
        let channel_count = first
            .channel_count
            .ok_or_else(|| Error::bitstream("AAC channel count was not found in first segment"))?;
        let asc = first.audio_specific_config.clone().ok_or_else(|| {
            Error::bitstream("AAC AudioSpecificConfig was not found in first segment")
        })?;
        tracks.push(FragmentedTrack::audio(
            next_track_id,
            sample_rate,
            channel_count,
            asc,
        ));
    }

    for track in &mut tracks {
        track.timescale = match &track.kind {
            crate::mp4::FragmentedTrackKind::Wvtt => track.timescale,
            crate::mp4::FragmentedTrackKind::Video { .. } => {
                first.video_timescale.unwrap_or(90_000)
            }
            crate::mp4::FragmentedTrackKind::Audio { sample_rate, .. } => {
                first.audio_timescale.unwrap_or(*sample_rate)
            }
        };
    }
    if tracks.is_empty() {
        return Err(Error::invalid("first segment produced no tracks"));
    }
    Ok(tracks)
}
fn track_report(track: &FragmentedTrack, sample_count: usize, duration: u64) -> crate::TrackInfo {
    use crate::mp4::FragmentedTrackKind;
    let (track_type, codec, width, height, sample_rate, channel_count) = match &track.kind {
        FragmentedTrackKind::Wvtt => unreachable!("subtitle reports use OutputTrackInfo"),
        FragmentedTrackKind::Video {
            width,
            height,
            codec,
        } => (
            crate::TrackType::Video,
            codec.codec(),
            Some(*width),
            Some(*height),
            None,
            None,
        ),
        FragmentedTrackKind::Audio {
            sample_rate,
            channel_count,
            ..
        } => (
            crate::TrackType::Audio,
            crate::Codec::Aac,
            None,
            None,
            Some(*sample_rate),
            Some(*channel_count),
        ),
    };
    crate::TrackInfo {
        track_type,
        codec,
        timescale: track.timescale,
        duration,
        sample_count,
        width,
        height,
        sample_rate,
        channel_count,
    }
}
fn packet_sample(
    packet: EncodedPacket,
    timescale: u32,
    base_90k: u64,
    origin: Option<(i128, u32)>,
) -> Result<Mp4Sample> {
    let (dts, pts, duration) = if let Some(time) = packet.timing.filter(|_| origin.is_some()) {
        let (base, base_scale) = origin.unwrap_or((i128::from(base_90k), 90_000));
        let scale = |value: i128| -> Result<i128> {
            let denominator = i128::from(time.timescale) * i128::from(base_scale);
            if denominator == 0 {
                return Err(Error::muxing("zero timestamp timescale"));
            }
            value
                .checked_mul(i128::from(base_scale))
                .and_then(|v| {
                    base.checked_mul(i128::from(time.timescale))
                        .and_then(|base| v.checked_sub(base))
                })
                .and_then(|v| v.checked_mul(i128::from(timescale)))
                .map(|v| v / denominator)
                .ok_or_else(|| Error::muxing("timestamp conversion overflow"))
        };
        (
            scale(time.dts)?,
            scale(time.pts)?,
            i128::from(time.duration) * i128::from(timescale) / i128::from(time.timescale),
        )
    } else {
        let scale = |value: i128| value * i128::from(timescale) / 90_000;
        (
            scale(i128::from(packet.dts_90k) - i128::from(base_90k)),
            scale(packet.pts_90k - i128::from(base_90k)),
            if let Some(time) = packet.timing {
                i128::from(time.duration) * i128::from(timescale) / i128::from(time.timescale)
            } else if matches!(packet.kind, StreamKind::Aac) {
                i128::from(packet.duration)
            } else {
                scale(i128::from(packet.duration))
            },
        )
    };
    let data = if packet.is_length_prefixed || matches!(packet.kind, StreamKind::Aac) {
        packet.data
    } else {
        avc::annex_b_to_length_prefixed(&packet.data)?
    };
    let data = if matches!(packet.kind, StreamKind::Avc | StreamKind::Hevc) {
        crate::codecs::strip_video_parameters(data, packet.kind)?
    } else {
        data
    };
    Ok(Mp4Sample {
        source: None,
        data,
        dts: u64::try_from(dts).map_err(|_| Error::muxing("DTS precedes common decode origin"))?,
        pts,
        duration: u32::try_from(duration).map_err(|_| Error::muxing("sample duration overflow"))?,
        is_key: packet.is_key,
        offset: 0,
    })
}
fn check_media_config(previous: &mut Option<DemuxOutput>, current: &DemuxOutput) -> Result<()> {
    if let Some(old) = previous {
        if old.saw_video != current.saw_video
            || old.saw_audio != current.saw_audio
            || old.video_timescale != current.video_timescale
            || old.audio_timescale != current.audio_timescale
            || current.width.is_some_and(|v| old.width != Some(v))
            || current.height.is_some_and(|v| old.height != Some(v))
            || old.sample_rate != current.sample_rate
            || old.channel_count != current.channel_count
            || current
                .sps
                .as_ref()
                .is_some_and(|v| old.sps.as_ref() != Some(v))
            || current
                .pps
                .as_ref()
                .is_some_and(|v| old.pps.as_ref() != Some(v))
            || current
                .vps
                .as_ref()
                .is_some_and(|v| old.vps.as_ref() != Some(v))
            || current
                .audio_specific_config
                .as_ref()
                .is_some_and(|v| old.audio_specific_config.as_ref() != Some(v))
        {
            return Err(Error::unsupported(
                "mid-stream track or codec configuration changed",
            ));
        }
    } else {
        *previous = Some(DemuxOutput {
            resource_digest: None,
            map_digest: None,
            packed_anchor: None,
            packets: Vec::new(),
            video_timescale: current.video_timescale,
            audio_timescale: current.audio_timescale,
            saw_video: current.saw_video,
            saw_audio: current.saw_audio,
            vps: current.vps.clone(),
            sps: current.sps.clone(),
            pps: current.pps.clone(),
            width: current.width,
            height: current.height,
            audio_specific_config: current.audio_specific_config.clone(),
            sample_rate: current.sample_rate,
            channel_count: current.channel_count,
        });
    }
    Ok(())
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresentationRange {
    pub(super) start: MediaTime,
    pub(super) end: MediaTime,
}
impl PresentationRange {
    pub fn new(start: MediaTime, end: MediaTime) -> EngineResult<Self> {
        if start.ticks < 0 || cmp(start, end)? != Ordering::Less {
            return Err(fail(EngineErrorKind::InvalidOptions));
        }
        Ok(Self { start, end })
    }
    pub fn start(&self) -> MediaTime {
        self.start
    }
    pub fn end(&self) -> MediaTime {
        self.end
    }
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum GapPolicy {
    #[default]
    Preserve,
    Collapse,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum TimelineChangePolicy {
    #[default]
    Fail,
    Split,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum TailDurationPolicy {
    #[default]
    RequireEvidence,
    Explicit(MediaTime),
}
fn cmp(a: MediaTime, b: MediaTime) -> EngineResult<Ordering> {
    let (sa, sb) = (i128::from(a.timescale), i128::from(b.timescale));
    let whole = a.ticks.div_euclid(sa).cmp(&b.ticks.div_euclid(sb));
    if whole != Ordering::Equal {
        return Ok(whole);
    }
    Ok((a.ticks.rem_euclid(sa) * sb).cmp(&(b.ticks.rem_euclid(sb) * sa)))
}
fn add(a: MediaTime, b: MediaTime) -> EngineResult<MediaTime> {
    combine(a, b, false)
}
fn sub(a: MediaTime, b: MediaTime) -> EngineResult<MediaTime> {
    combine(a, b, true)
}
fn reduced(time: MediaTime) -> MediaTime {
    let mut a = time.timescale;
    let mut b = time.ticks.rem_euclid(i128::from(time.timescale)) as u32;
    while b != 0 {
        (a, b) = (b, a % b);
    }
    MediaTime {
        ticks: time.ticks / i128::from(a),
        timescale: time.timescale / a,
    }
}
fn combine(a: MediaTime, b: MediaTime, subtract: bool) -> EngineResult<MediaTime> {
    let (a, b) = (reduced(a), reduced(b));
    fn gcd(mut a: u32, mut b: u32) -> u32 {
        while b != 0 {
            (a, b) = (b, a % b);
        }
        a
    }
    let common = gcd(a.timescale, b.timescale);
    let scale = (a.timescale / common)
        .checked_mul(b.timescale)
        .ok_or_else(|| fail(EngineErrorKind::TimeOverflow))?;
    let left = a.ticks.checked_mul(i128::from(scale / a.timescale));
    let right = b.ticks.checked_mul(i128::from(scale / b.timescale));
    let ticks = left
        .zip(right)
        .and_then(|(a, b)| {
            if subtract {
                a.checked_sub(b)
            } else {
                a.checked_add(b)
            }
        })
        .ok_or_else(|| fail(EngineErrorKind::TimeOverflow))?;
    Ok(reduced(MediaTime {
        ticks,
        timescale: scale,
    }))
}
fn scale(value: MediaTime, scale: u32) -> EngineResult<i128> {
    let value = reduced(value);
    let mut a = value.timescale;
    let mut b = scale;
    while b != 0 {
        (a, b) = (b, a % b);
    }
    let denominator = i128::from(value.timescale / a);
    let numerator = i128::from(scale / a);
    (value.ticks / denominator)
        .checked_mul(numerator)
        .and_then(|whole| whole.checked_add((value.ticks % denominator) * numerator / denominator))
        .ok_or_else(|| fail(EngineErrorKind::TimeOverflow))
}
fn independent(packet: &EncodedPacket) -> EngineResult<bool> {
    if matches!(packet.kind, StreamKind::Aac) {
        return Ok(true);
    }
    let bytes = if packet.is_length_prefixed {
        packet.data.clone()
    } else {
        avc::annex_b_to_length_prefixed(&packet.data).map_err(media_error)?
    };
    let mut offset = 0;
    let mut safe = false;
    while offset < bytes.len() {
        let prefix = bytes
            .get(offset..offset + 4)
            .ok_or_else(|| fail(EngineErrorKind::Media))?;
        let length = u32::from_be_bytes(prefix.try_into().unwrap()) as usize;
        offset += 4;
        let end = offset
            .checked_add(length)
            .ok_or_else(|| fail(EngineErrorKind::TimeOverflow))?;
        let nal = bytes
            .get(offset..end)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| fail(EngineErrorKind::Media))?;
        safe |= match packet.kind {
            StreamKind::Avc => nal[0] & 31 == 5,
            StreamKind::Hevc => matches!((nal[0] >> 1) & 63, 16..=20),
            _ => false,
        };
        offset = end;
    }
    Ok(safe)
}
fn parse_pdt(text: &str) -> EngineResult<MediaTime> {
    let invalid = || fail(EngineErrorKind::TimelineAmbiguous);
    let n = |start: usize, end: usize| {
        let digits = text.get(start..end).ok_or_else(invalid)?;
        if !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid());
        }
        digits.parse::<i128>().map_err(|_| invalid())
    };
    let (year, month, day, hour, minute, second) = (
        n(0, 4)?,
        n(5, 7)?,
        n(8, 10)?,
        n(11, 13)?,
        n(14, 16)?,
        n(17, 19)?,
    );
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let month_days = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if !text.is_ascii()
        || text.as_bytes().get(4) != Some(&b'-')
        || text.as_bytes().get(7) != Some(&b'-')
        || !matches!(text.as_bytes().get(10), Some(b'T' | b't'))
        || text.as_bytes().get(13) != Some(&b':')
        || text.as_bytes().get(16) != Some(&b':')
        || !(1..=12).contains(&month)
        || !(1..=month_days).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return Err(invalid());
    }
    let y = year - i128::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = month + if month > 2 { -3 } else { 9 };
    let days =
        era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + (153 * m + 2) / 5 + day - 1 - 719468;
    let mut rest = text.get(19..).ok_or_else(invalid)?;
    let mut nano = 0;
    if rest.starts_with('.') {
        rest = &rest[1..];
        let length = rest.bytes().take_while(u8::is_ascii_digit).count();
        if length == 0 || length > 9 {
            return Err(invalid());
        }
        nano = rest[..length].parse::<i128>().map_err(|_| invalid())?
            * 10i128.pow((9 - length) as u32);
        rest = &rest[length..];
    }
    let offset = if matches!(rest, "Z" | "z") {
        0
    } else {
        if rest.len() != 6
            || !matches!(rest.as_bytes()[0], b'+' | b'-')
            || rest.as_bytes()[3] != b':'
            || !rest.as_bytes()[1..3].iter().all(u8::is_ascii_digit)
            || !rest.as_bytes()[4..6].iter().all(u8::is_ascii_digit)
        {
            return Err(invalid());
        }
        let h = rest[1..3].parse::<i128>().map_err(|_| invalid())?;
        let m = rest[4..6].parse::<i128>().map_err(|_| invalid())?;
        if h > 23 || m > 59 {
            return Err(invalid());
        }
        (h * 3600 + m * 60) * if rest.starts_with('-') { -1 } else { 1 }
    };
    Ok(MediaTime {
        ticks: ((days * 24 + hour) * 3600 + minute * 60 + second - offset) * 1_000_000_000 + nano,
        timescale: 1_000_000_000,
    })
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum OutputFormat {
    /// Standard non-fragmented MP4 (`ftyp` + `moov` + `mdat`).
    /// Batch pipeline: all segments are demuxed into memory before muxing.
    /// Fastest for small inputs; highest peak memory.
    #[default]
    Mp4,
    /// Fragmented MP4 / CMAF (`ftyp` + `moov` + per-segment `moof` + `mdat`).
    /// Streaming pipeline: each segment is written to disk as it's demuxed.
    /// Interruptible; produces fMP4 directly.
    FragmentedMp4,
    /// Standard non-fragmented MP4 via the streaming fragmented pipeline.
    /// Each segment is demuxed and written to a temp fMP4 file, then
    /// finalized into faststart MP4. Output uses the same classic sample tables
    /// as [`Mp4`](Self::Mp4). Native file output defaults to copying media into
    /// a compact file; optional InPlace finalization fills reserved header space
    /// or moves media within the temporary file. Both retain a sample index. The
    /// temp file (`<output>.partial.<ext>`) is a playable fMP4 if the
    /// process is interrupted before finalization.
    StreamingMp4,
}

pub use crate::capabilities::{CapabilityDecision, CapabilityQuery, query_capability};
#[cfg(feature = "serde")]
mod wire;

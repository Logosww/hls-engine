use crate::codecs::avc;
use crate::error::{Error, Result};
use crate::types::{Codec, TrackInfo, TrackType};

/// Movie (mvhd) and tkhd timescale. 57600 is the LCM of common frame rates
/// (24/25/30/60/120/240 fps), so rescaling track durations into it introduces
/// no rounding for typical content.
const MOVIE_TIMESCALE: u32 = 57_600;

#[derive(Debug, Clone)]
pub(crate) struct Mp4Sample {
    pub data: Vec<u8>,
    /// File-backed payload; absent for the in-memory APIs.
    pub source: Option<(u64, u32)>,
    pub dts: u64,
    pub pts: u64,
    pub duration: u32,
    pub is_key: bool,
    pub(crate) offset: u64,
}

impl Mp4Sample {
    fn size(&self) -> u64 {
        self.source
            .map_or(self.data.len() as u64, |(_, size)| u64::from(size))
    }
}

#[derive(Debug, Clone)]
pub(crate) enum VideoCodec {
    Avc { avcc: Vec<u8> },
    Hevc { hvcc: Vec<u8> },
}

impl VideoCodec {
    pub(crate) fn codec(&self) -> Codec {
        match self {
            Self::Avc { .. } => Codec::Avc,
            Self::Hevc { .. } => Codec::Hevc,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum Mp4Track {
    Video {
        samples: Vec<Mp4Sample>,
        timescale: u32,
        width: u16,
        height: u16,
        codec: VideoCodec,
    },
    Audio {
        samples: Vec<Mp4Sample>,
        timescale: u32,
        sample_rate: u32,
        channel_count: u8,
        audio_specific_config: Vec<u8>,
    },
}

impl Mp4Track {
    pub(crate) fn samples(&self) -> &[Mp4Sample] {
        match self {
            Self::Video { samples, .. } | Self::Audio { samples, .. } => samples,
        }
    }

    fn samples_mut(&mut self) -> &mut [Mp4Sample] {
        match self {
            Self::Video { samples, .. } | Self::Audio { samples, .. } => samples,
        }
    }

    fn track_info(&self) -> TrackInfo {
        match self {
            Self::Video {
                samples,
                timescale,
                width,
                height,
                codec,
                ..
            } => TrackInfo {
                track_type: TrackType::Video,
                codec: codec.codec(),
                timescale: *timescale,
                duration: track_duration(samples),
                sample_count: samples.len(),
                width: Some(*width),
                height: Some(*height),
                sample_rate: None,
                channel_count: None,
            },
            Self::Audio {
                samples,
                timescale,
                sample_rate,
                channel_count,
                ..
            } => TrackInfo {
                track_type: TrackType::Audio,
                codec: Codec::Aac,
                timescale: *timescale,
                duration: track_duration(samples),
                sample_count: samples.len(),
                width: None,
                height: None,
                sample_rate: Some(*sample_rate),
                channel_count: Some(*channel_count),
            },
        }
    }
}

pub(crate) struct Mp4Muxer {
    tracks: Vec<Mp4Track>,
}

impl Mp4Muxer {
    pub(crate) fn new(tracks: Vec<Mp4Track>) -> Self {
        Self { tracks }
    }

    pub(crate) fn write_checked(
        self,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<(Vec<u8>, Vec<TrackInfo>)> {
        let mut output = Vec::new();
        let (_, infos) = self.write_to(&mut output, check, |sample, output| {
            output.extend_from_slice(&sample.data);
            Ok(())
        })?;
        Ok((output, infos))
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn write_file(
        self,
        input: &mut std::fs::File,
        output: &mut std::fs::File,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<(u64, Vec<TrackInfo>)> {
        use std::io::{Read, Seek, SeekFrom, Write};
        let mut buffer = vec![0; 1024 * 1024];
        self.write_to(output, check, |sample, output| {
            let (offset, size) = sample
                .source
                .ok_or_else(|| Error::muxing("missing sample source"))?;
            input.seek(SeekFrom::Start(offset))?;
            let mut remaining = u64::from(size);
            while remaining > 0 {
                check()?;
                let n = remaining.min(buffer.len() as u64) as usize;
                input.read_exact(&mut buffer[..n])?;
                output.write_all(&buffer[..n])?;
                remaining -= n as u64;
            }
            Ok(())
        })
    }

    fn write_to<W: std::io::Write>(
        mut self,
        output: &mut W,
        check: &dyn Fn() -> Result<()>,
        mut payload: impl FnMut(&Mp4Sample, &mut W) -> Result<()>,
    ) -> Result<(u64, Vec<TrackInfo>)> {
        check()?;
        if self.tracks.is_empty() {
            return Err(Error::muxing("MP4 output requires at least one track"));
        }
        // Validate before table construction so all timeline additions are safe.
        for track in &self.tracks {
            if track_timescale(track) == 0 {
                return Err(Error::muxing("zero track timescale"));
            }
            fit_u32(track.samples().len() as u64, "sample count")?;
            if track
                .samples()
                .windows(2)
                .any(|pair| pair[1].dts <= pair[0].dts)
            {
                return Err(Error::muxing(
                    "sample DTS must be strictly increasing; discontinuities are unsupported",
                ));
            }
            for sample in track.samples() {
                check()?;
                fit_u32(sample.size(), "sample size")?;
                sample
                    .dts
                    .checked_add(u64::from(sample.duration))
                    .ok_or_else(|| Error::muxing("DTS overflow"))?;
                sample
                    .pts
                    .checked_add(u64::from(sample.duration))
                    .ok_or_else(|| Error::muxing("PTS overflow"))?;
            }
            presentation_span(track.samples())
                .checked_add(start_offset(track.samples()))
                .ok_or_else(|| Error::muxing("track duration overflow"))?;
        }
        let ftyp = ftyp_box(&self.tracks);
        let chunks: Vec<_> = self.tracks.iter().map(split_chunks).collect();
        let mut placed = Vec::new();
        for (ti, track_chunks) in chunks.iter().enumerate() {
            fit_u32(track_chunks.len() as u64, "chunk count")?;
            for (ci, chunk) in track_chunks.iter().enumerate() {
                placed.push((ti, ci, chunk.first_sample));
            }
        }
        // Compare rational timestamps exactly, without rescaling or rounding.
        placed.sort_by(|&(a, ac, ai), &(b, bc, bi)| {
            let left = u128::from(self.tracks[a].samples()[ai].dts)
                * u128::from(track_timescale(&self.tracks[b]));
            let right = u128::from(self.tracks[b].samples()[bi].dts)
                * u128::from(track_timescale(&self.tracks[a]));
            (left, a, ac).cmp(&(right, b, bc))
        });
        let media_size =
            self.tracks
                .iter()
                .flat_map(Mp4Track::samples)
                .try_fold(0u64, |sum, s| {
                    sum.checked_add(s.size())
                        .ok_or_else(|| Error::muxing("media size overflow"))
                })?;
        let header = mdat_header(media_size)?;
        for track in &mut self.tracks {
            for sample in track.samples_mut() {
                sample.offset = 0;
            }
        }
        let mut moov = moov_box(&self.tracks, &chunks)?;
        // stco -> co64 only grows tables; another pass accounts for that growth.
        loop {
            check()?;
            let mut offset = (ftyp.len() as u64)
                .checked_add(moov.len() as u64)
                .and_then(|v| v.checked_add(header.len() as u64))
                .ok_or_else(|| Error::muxing("layout overflow"))?;
            for &(ti, ci, _) in &placed {
                let chunk = chunks[ti][ci];
                for sample in &mut self.tracks[ti].samples_mut()
                    [chunk.first_sample..chunk.first_sample + chunk.sample_count as usize]
                {
                    sample.offset = offset;
                    offset = offset
                        .checked_add(sample.size())
                        .ok_or_else(|| Error::muxing("sample offset overflow"))?;
                }
            }
            let next = moov_box(&self.tracks, &chunks)?;
            let stable = next.len() == moov.len();
            moov = next;
            if stable {
                break;
            }
        }
        check()?;
        output.write_all(&ftyp)?;
        output.write_all(&moov)?;
        output.write_all(&header)?;
        for &(ti, ci, _) in &placed {
            let chunk = chunks[ti][ci];
            for sample in &self.tracks[ti].samples()
                [chunk.first_sample..chunk.first_sample + chunk.sample_count as usize]
            {
                check()?;
                payload(sample, output)?;
            }
        }
        let total = (ftyp.len() as u64)
            .checked_add(moov.len() as u64)
            .and_then(|v| v.checked_add(header.len() as u64))
            .and_then(|v| v.checked_add(media_size))
            .ok_or_else(|| Error::muxing("output size overflow"))?;
        Ok((
            total,
            self.tracks.iter().map(Mp4Track::track_info).collect(),
        ))
    }
}

fn mdat_header(payload: u64) -> Result<Vec<u8>> {
    let size = payload
        .checked_add(8)
        .ok_or_else(|| Error::muxing("mdat size overflow"))?;
    let mut out = Vec::new();
    if let Ok(size) = u32::try_from(size) {
        be_u32(&mut out, size);
        out.extend_from_slice(b"mdat");
    } else {
        be_u32(&mut out, 1);
        out.extend_from_slice(b"mdat");
        be_u64(
            &mut out,
            payload
                .checked_add(16)
                .ok_or_else(|| Error::muxing("mdat size overflow"))?,
        );
    }
    Ok(out)
}

pub(crate) fn make_video_track(
    mut raw_samples: Vec<Mp4Sample>,
    sps: &[u8],
    pps: &[u8],
) -> Result<Mp4Track> {
    if raw_samples.is_empty() {
        return Err(Error::muxing("video track contains no samples"));
    }
    let info = avc::parse_sps(sps)?;
    let avcc = avc::avcc(sps, pps)?;
    assign_delta_durations(&mut raw_samples)?;

    Ok(Mp4Track::Video {
        samples: raw_samples,
        timescale: 90_000,
        width: info.width,
        height: info.height,
        codec: VideoCodec::Avc { avcc },
    })
}

pub(crate) fn make_hevc_video_track(
    mut raw_samples: Vec<Mp4Sample>,
    vps: &[u8],
    sps: &[u8],
    pps: &[u8],
) -> Result<Mp4Track> {
    use crate::codecs::hevc;
    if raw_samples.is_empty() {
        return Err(Error::muxing("video track contains no samples"));
    }
    let info = hevc::parse_sps(sps)?;
    let hvcc = hevc::hvcc(vps, sps, pps)?;
    assign_delta_durations(&mut raw_samples)?;

    Ok(Mp4Track::Video {
        samples: raw_samples,
        timescale: 90_000,
        width: info.width,
        height: info.height,
        codec: VideoCodec::Hevc { hvcc },
    })
}

pub(crate) fn make_audio_track(
    mut samples: Vec<Mp4Sample>,
    sample_rate: u32,
    channel_count: u8,
    audio_specific_config: Vec<u8>,
) -> Result<Mp4Track> {
    if samples.is_empty() {
        return Err(Error::muxing("audio track contains no samples"));
    }
    for sample in &mut samples {
        sample.duration = 1024;
    }

    Ok(Mp4Track::Audio {
        samples,
        timescale: sample_rate,
        sample_rate,
        channel_count,
        audio_specific_config,
    })
}

pub(crate) fn assign_delta_durations(samples: &mut [Mp4Sample]) -> Result<()> {
    samples.sort_by_key(|sample| sample.dts);
    let mut previous_delta = 3000_u32;
    for index in 0..samples.len() {
        let duration = if let Some(next) = samples.get(index + 1) {
            if next.dts <= samples[index].dts {
                return Err(Error::muxing(
                    "video sample DTS must be strictly increasing",
                ));
            }
            u32::try_from(next.dts - samples[index].dts)
                .map_err(|_| Error::muxing("video sample duration exceeds u32"))?
        } else {
            previous_delta
        };
        samples[index].duration = duration;
        previous_delta = duration;
    }
    Ok(())
}

fn track_duration(samples: &[Mp4Sample]) -> u64 {
    samples
        .last()
        .map(|sample| sample.dts + u64::from(sample.duration))
        .unwrap_or(0)
}

/// Presentation span in the track's timescale: max(PTS + duration) − min(PTS).
/// PTS-based (unlike `track_duration` which is DTS-based) so it reflects the
/// true visible duration, including the tail of B-frame reordering.
fn presentation_span(samples: &[Mp4Sample]) -> u64 {
    if samples.is_empty() {
        return 0;
    }
    let min_pts = samples.iter().map(|s| s.pts).min().unwrap_or(0);
    let max_end = samples
        .iter()
        .map(|s| s.pts + u64::from(s.duration))
        .max()
        .unwrap_or(0);
    max_end.saturating_sub(min_pts)
}

/// Initial PTS of the track in the track's timescale. Used to build an edit
/// list when the first sample doesn't start at PTS 0 (e.g. non-aligned HLS
/// segments).
fn start_offset(samples: &[Mp4Sample]) -> u64 {
    samples.first().map(|s| s.pts).unwrap_or(0)
}

fn ftyp_box(tracks: &[Mp4Track]) -> Vec<u8> {
    // Compatible brands are emitted conditionally based on the codecs that
    // actually appear in the file — writing `avc1`/`hvc1` for an audio-only
    // file is technically valid but misleading.
    let has_avc = tracks.iter().any(|t| {
        matches!(
            t,
            Mp4Track::Video {
                codec: VideoCodec::Avc { .. },
                ..
            }
        )
    });
    let has_hevc = tracks.iter().any(|t| {
        matches!(
            t,
            Mp4Track::Video {
                codec: VideoCodec::Hevc { .. },
                ..
            }
        )
    });
    boxed(b"ftyp", |out| {
        out.extend_from_slice(b"isom");
        be_u32(out, 0x200);
        out.extend_from_slice(b"isom");
        if has_avc {
            out.extend_from_slice(b"avc1");
        }
        if has_hevc {
            out.extend_from_slice(b"hvc1");
        }
        out.extend_from_slice(b"mp41");
    })
}

/// ftyp for fragmented MP4 / CMAF. Uses `iso5` as major brand because
/// QuickTime checks the major brand to decide whether the file is a
/// fragmented MP4 and whether to show playback controls.
fn fragmented_ftyp_box() -> Vec<u8> {
    boxed(b"ftyp", |out| {
        out.extend_from_slice(b"iso5");
        be_u32(out, 0x200);
        out.extend_from_slice(b"iso5");
        out.extend_from_slice(b"iso6");
        out.extend_from_slice(b"mp41");
    })
}

fn moov_box(tracks: &[Mp4Track], chunks_per_track: &[Vec<ChunkMeta>]) -> Result<Vec<u8>> {
    let movie_timescale = MOVIE_TIMESCALE;
    // Movie duration = max over tracks of (start_offset + presentation_span),
    // all rescaled into the movie timescale. Using PTS span (not DTS-based
    // track_duration) correctly accounts for B-frame composition offsets and
    // non-zero initial PTS.
    let movie_duration = tracks
        .iter()
        .map(|track| {
            let ts = track_timescale(track);
            let span = presentation_span(track.samples());
            let offset = start_offset(track.samples());
            rescale(span + offset, ts, movie_timescale)
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .max()
        .unwrap_or(0);

    boxed_result(b"moov", |out| {
        out.extend_from_slice(&mvhd_box(movie_timescale, movie_duration, tracks.len())?);
        for (index, track) in tracks.iter().enumerate() {
            out.extend_from_slice(&trak_box(
                track,
                (index + 1) as u32,
                movie_timescale,
                &chunks_per_track[index],
            )?);
        }
        Ok(())
    })
}

fn trak_box(
    track: &Mp4Track,
    track_id: u32,
    movie_timescale: u32,
    chunks: &[ChunkMeta],
) -> Result<Vec<u8>> {
    let ts = track_timescale(track);
    let span = presentation_span(track.samples());
    let offset = start_offset(track.samples());
    // tkhd duration includes the initial PTS offset so the track's movie
    // timeline matches mvhd. mdhd duration uses just the presentation span
    // (media-local time, starting at 0).
    let tkhd_duration = rescale(span + offset, ts, movie_timescale)?;
    let offset_movie = rescale(offset, ts, movie_timescale)?;
    let span_movie = rescale(span, ts, movie_timescale)?;

    boxed_result(b"trak", |out| {
        out.extend_from_slice(&tkhd_box(track, track_id, tkhd_duration)?);
        // elst is only needed when the first sample's PTS > 0 (e.g. an HLS
        // segment that doesn't start at PTS 0). The standard "empty edit +
        // real edit" pair shifts the movie timeline past the initial gap.
        if offset_movie > 0 {
            out.extend_from_slice(&edts_box(offset_movie, span_movie)?);
        }
        out.extend_from_slice(&mdia_box(track, span, chunks)?);
        Ok(())
    })
}

/// Edit list box. Two entries: an empty edit (media_time = -1) covering the
/// initial PTS gap, followed by the real edit (media_time = 0) for the
/// presentation span. Per ISO/IEC 14496-12.
fn edts_box(empty_duration: u64, real_duration: u64) -> Result<Vec<u8>> {
    let wide = empty_duration.max(real_duration) > u64::from(u32::MAX);
    let elst = full_box_result(b"elst", u8::from(wide), 0, |out| {
        be_u32(out, 2); // entry_count
        // Empty edit: hold for empty_duration, media_time = -1 (no media).
        if wide {
            be_u64(out, empty_duration);
            be_u64(out, u64::MAX);
        } else {
            be_u32(out, empty_duration as u32);
            be_i32(out, -1);
        }
        be_u32(out, 0x0001_0000); // media_rate 1.0
        // Real edit: play the media for real_duration starting at media_time 0.
        if wide {
            be_u64(out, real_duration);
            be_u64(out, 0);
        } else {
            be_u32(out, real_duration as u32);
            be_u32(out, 0);
        }
        be_u32(out, 0x0001_0000);
        Ok(())
    })?;
    boxed_result(b"edts", |out| {
        out.extend_from_slice(&elst);
        Ok(())
    })
}

fn mdia_box(track: &Mp4Track, media_duration: u64, chunks: &[ChunkMeta]) -> Result<Vec<u8>> {
    boxed_result(b"mdia", |out| {
        out.extend_from_slice(&mdhd_box(track_timescale(track), media_duration)?);
        out.extend_from_slice(&hdlr_box(track));
        out.extend_from_slice(&minf_box(track, chunks)?);
        Ok(())
    })
}

fn minf_box(track: &Mp4Track, chunks: &[ChunkMeta]) -> Result<Vec<u8>> {
    boxed_result(b"minf", |out| {
        match track {
            Mp4Track::Video { .. } => out.extend_from_slice(&vmhd_box()),
            Mp4Track::Audio { .. } => out.extend_from_slice(&smhd_box()),
        }
        out.extend_from_slice(&dinf_box());
        out.extend_from_slice(&stbl_box(track, chunks)?);
        Ok(())
    })
}

fn stbl_box(track: &Mp4Track, chunks: &[ChunkMeta]) -> Result<Vec<u8>> {
    let needs_ctts = track
        .samples()
        .iter()
        .any(|sample| sample.pts != sample.dts);
    let all_key = track.samples().iter().all(|sample| sample.is_key);
    boxed_result(b"stbl", |out| {
        out.extend_from_slice(&stsd_box(track)?);
        out.extend_from_slice(&stts_box(track.samples()));
        if needs_ctts {
            out.extend_from_slice(&ctts_box(track.samples())?);
            // cslg helps players handle B-frame CTS offsets, especially
            // when some offsets are negative (QuickTime relies on this).
            out.extend_from_slice(&cslg_box(track.samples())?);
        }
        // stss is omitted when every sample is a key frame — the spec
        // treats absence as "all sync", which is more compact.
        if matches!(track, Mp4Track::Video { .. }) && !all_key {
            out.extend_from_slice(&stss_box(track.samples()));
        }
        out.extend_from_slice(&stsc_box(chunks));
        out.extend_from_slice(&stsz_box(track.samples())?);
        out.extend_from_slice(&stco_box(track.samples(), chunks)?);
        Ok(())
    })
}

fn mvhd_box(timescale: u32, duration: u64, track_count: usize) -> Result<Vec<u8>> {
    let creation = creation_time();
    let wide = duration > u64::from(u32::MAX);
    full_box_result(b"mvhd", u8::from(wide), 0, |out| {
        if wide {
            be_u64(out, u64::from(creation));
            be_u64(out, u64::from(creation));
        } else {
            be_u32(out, creation);
            be_u32(out, creation);
        }
        be_u32(out, timescale);
        if wide {
            be_u64(out, duration);
        } else {
            be_u32(out, duration as u32);
        }
        be_u32(out, 0x0001_0000);
        be_u16(out, 0x0100);
        be_u16(out, 0);
        be_u32(out, 0);
        be_u32(out, 0);
        unity_matrix(out);
        for _ in 0..6 {
            be_u32(out, 0);
        }
        be_u32(out, fit_u32(track_count as u64 + 1, "next_track_id")?);
        Ok(())
    })
}

fn tkhd_box(track: &Mp4Track, track_id: u32, duration: u64) -> Result<Vec<u8>> {
    let creation = creation_time();
    let wide = duration > u64::from(u32::MAX);
    full_box_result(b"tkhd", u8::from(wide), 0x000007, |out| {
        if wide {
            be_u64(out, u64::from(creation));
            be_u64(out, u64::from(creation));
        } else {
            be_u32(out, creation);
            be_u32(out, creation);
        }
        be_u32(out, track_id);
        be_u32(out, 0);
        if wide {
            be_u64(out, duration);
        } else {
            be_u32(out, duration as u32);
        }
        be_u32(out, 0);
        be_u32(out, 0);
        be_u16(out, 0);
        be_u16(out, 0);
        be_u16(
            out,
            if matches!(track, Mp4Track::Audio { .. }) {
                0x0100
            } else {
                0
            },
        );
        be_u16(out, 0);
        unity_matrix(out);
        match track {
            Mp4Track::Video { width, height, .. } => {
                be_u32(out, u32::from(*width) << 16);
                be_u32(out, u32::from(*height) << 16);
            }
            Mp4Track::Audio { .. } => {
                be_u32(out, 0);
                be_u32(out, 0);
            }
        }
        Ok(())
    })
}

fn mdhd_box(timescale: u32, duration: u64) -> Result<Vec<u8>> {
    let creation = creation_time();
    let wide = duration > u64::from(u32::MAX);
    full_box_result(b"mdhd", u8::from(wide), 0, |out| {
        if wide {
            be_u64(out, u64::from(creation));
            be_u64(out, u64::from(creation));
        } else {
            be_u32(out, creation);
            be_u32(out, creation);
        }
        be_u32(out, timescale);
        if wide {
            be_u64(out, duration);
        } else {
            be_u32(out, duration as u32);
        }
        be_u16(out, 0x55c4); // und
        be_u16(out, 0);
        Ok(())
    })
}

/// MP4 creation time as seconds since 1904-01-01 00:00:00 UTC (the MP4
/// epoch). Falls back to 0 if the wall clock is unavailable.
///
/// On `wasm32-unknown-unknown`, `SystemTime::now()` panics ("time not
/// implemented on this platform"), so we return 0 unconditionally. A
/// zero creation timestamp is harmless for playback.
fn creation_time() -> u32 {
    #[cfg(target_arch = "wasm32")]
    {
        0
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::time::{SystemTime, UNIX_EPOCH};
        // Seconds between 1904-01-01 and 1970-01-01.
        const EPOCH_OFFSET: u64 = 2_082_844_800;
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|d| EPOCH_OFFSET.checked_add(d.as_secs()))
            .and_then(|s| u32::try_from(s).ok())
            .unwrap_or(0)
    }
}

fn hdlr_box(track: &Mp4Track) -> Vec<u8> {
    full_box(b"hdlr", 0, 0, |out| {
        be_u32(out, 0);
        match track {
            Mp4Track::Video { .. } => out.extend_from_slice(b"vide"),
            Mp4Track::Audio { .. } => out.extend_from_slice(b"soun"),
        }
        be_u32(out, 0);
        be_u32(out, 0);
        be_u32(out, 0);
        match track {
            Mp4Track::Video { .. } => out.extend_from_slice(b"VideoHandler\0"),
            Mp4Track::Audio { .. } => out.extend_from_slice(b"SoundHandler\0"),
        }
    })
}

fn vmhd_box() -> Vec<u8> {
    full_box(b"vmhd", 0, 1, |out| {
        be_u16(out, 0);
        be_u16(out, 0);
        be_u16(out, 0);
        be_u16(out, 0);
    })
}

fn smhd_box() -> Vec<u8> {
    full_box(b"smhd", 0, 0, |out| {
        be_u16(out, 0);
        be_u16(out, 0);
    })
}

fn dinf_box() -> Vec<u8> {
    boxed(b"dinf", |out| {
        out.extend_from_slice(&full_box(b"dref", 0, 0, |dref| {
            be_u32(dref, 1);
            dref.extend_from_slice(&full_box(b"url ", 0, 1, |_| {}));
        }));
    })
}

fn stsd_box(track: &Mp4Track) -> Result<Vec<u8>> {
    full_box_result(b"stsd", 0, 0, |out| {
        be_u32(out, 1);
        match track {
            Mp4Track::Video {
                width,
                height,
                codec,
                ..
            } => match codec {
                VideoCodec::Avc { avcc } => {
                    out.extend_from_slice(&avc1_entry(*width, *height, avcc)?)
                }
                VideoCodec::Hevc { hvcc } => {
                    out.extend_from_slice(&hvc1_entry(*width, *height, hvcc)?)
                }
            },
            Mp4Track::Audio {
                sample_rate,
                channel_count,
                audio_specific_config,
                ..
            } => out.extend_from_slice(&mp4a_entry(
                *sample_rate,
                *channel_count,
                audio_specific_config,
            )?),
        }
        Ok(())
    })
}

fn avc1_entry(width: u16, height: u16, avcc: &[u8]) -> Result<Vec<u8>> {
    boxed_result(b"avc1", |out| {
        out.extend_from_slice(&[0; 6]);
        be_u16(out, 1);
        be_u16(out, 0);
        be_u16(out, 0);
        be_u32(out, 0);
        be_u32(out, 0);
        be_u32(out, 0);
        be_u16(out, width);
        be_u16(out, height);
        be_u32(out, 0x0048_0000);
        be_u32(out, 0x0048_0000);
        be_u32(out, 0);
        be_u16(out, 1);
        out.push(0);
        out.extend_from_slice(&[0; 31]);
        be_u16(out, 0x0018);
        be_u16(out, 0xffff);
        out.extend_from_slice(&boxed(b"avcC", |box_out| box_out.extend_from_slice(avcc)));
        Ok(())
    })
}

fn hvc1_entry(width: u16, height: u16, hvcc: &[u8]) -> Result<Vec<u8>> {
    boxed_result(b"hvc1", |out| {
        out.extend_from_slice(&[0; 6]);
        be_u16(out, 1);
        be_u16(out, 0);
        be_u16(out, 0);
        be_u32(out, 0);
        be_u32(out, 0);
        be_u32(out, 0);
        be_u16(out, width);
        be_u16(out, height);
        be_u32(out, 0x0048_0000);
        be_u32(out, 0x0048_0000);
        be_u32(out, 0);
        be_u16(out, 1);
        out.push(0);
        out.extend_from_slice(&[0; 31]);
        be_u16(out, 0x0018);
        be_u16(out, 0xffff);
        out.extend_from_slice(&boxed(b"hvcC", |box_out| box_out.extend_from_slice(hvcc)));
        Ok(())
    })
}

fn mp4a_entry(sample_rate: u32, channel_count: u8, asc: &[u8]) -> Result<Vec<u8>> {
    boxed_result(b"mp4a", |out| {
        out.extend_from_slice(&[0; 6]);
        be_u16(out, 1);
        be_u32(out, 0);
        be_u32(out, 0);
        be_u16(out, u16::from(channel_count));
        be_u16(out, 16);
        be_u16(out, 0);
        be_u16(out, 0);
        be_u32(out, sample_rate << 16);
        out.extend_from_slice(&esds_box(asc)?);
        Ok(())
    })
}

fn esds_box(asc: &[u8]) -> Result<Vec<u8>> {
    full_box_result(b"esds", 0, 0, |out| {
        // MPEG-4 descriptor length = size of content ONLY (not tag + length
        // bytes). A parent descriptor's content includes the FULL child
        // descriptor (tag + length_bytes + child_content). Many demuxers
        // (QuickTime, Chrome, ffmpeg) scan for tags and ignore the length
        // fields, but PotPlayer strictly follows them — undercounting causes
        // it to read AudioSpecificConfig from the wrong offset.
        let decoder_specific_len = asc.len();
        // DSI total = tag(1) + len_bytes + content
        let dsi_total = 1 + descriptor_len_size(decoder_specific_len) + decoder_specific_len;
        // DCD content = 13 fixed fields + full DSI
        let decoder_config_len = 13 + dsi_total;
        // DCD total = tag(1) + len_bytes + content
        let dcd_total = 1 + descriptor_len_size(decoder_config_len) + decoder_config_len;
        // SLC total = tag(1) + len_bytes + content(1)
        let slc_total = 1 + descriptor_len_size(1) + 1;
        // ES content = 3 (ES_ID + flags) + full DCD + full SLC
        let es_len = 3 + dcd_total + slc_total;

        descriptor(out, 0x03, es_len)?;
        be_u16(out, 1);
        out.push(0);

        descriptor(out, 0x04, decoder_config_len)?;
        out.push(0x40); // MPEG-4 Audio
        out.push(0x15); // AudioStream
        be_u24(out, 0);
        be_u32(out, 0);
        be_u32(out, 0);

        descriptor(out, 0x05, decoder_specific_len)?;
        out.extend_from_slice(asc);

        descriptor(out, 0x06, 1)?;
        out.push(2);
        Ok(())
    })
}

fn descriptor(out: &mut Vec<u8>, tag: u8, len: usize) -> Result<()> {
    out.push(tag);
    write_descriptor_len(out, len)
}

fn descriptor_len_size(_len: usize) -> usize {
    // Always use the 4-byte extended form (0x80 0x80 0x80 <len>). This
    // matches ffmpeg/MP4Box output and avoids interop issues with demuxers
    // (e.g. PotPlayer) that misparse the minimal 1-byte length form.
    4
}

fn write_descriptor_len(out: &mut Vec<u8>, len: usize) -> Result<()> {
    if len >= 0x1000_0000 {
        return Err(Error::muxing("ES descriptor is too large"));
    }
    let size = descriptor_len_size(len);
    for index in (0..size).rev() {
        let mut byte = ((len >> (index * 7)) & 0x7f) as u8;
        if index != 0 {
            byte |= 0x80;
        }
        out.push(byte);
    }
    Ok(())
}

fn stts_box(samples: &[Mp4Sample]) -> Vec<u8> {
    full_box(b"stts", 0, 0, |out| {
        let entries = grouped_counts(samples.iter().map(|sample| sample.duration));
        be_u32(out, entries.len() as u32);
        for (count, duration) in entries {
            be_u32(out, count);
            be_u32(out, duration);
        }
    })
}

fn ctts_box(samples: &[Mp4Sample]) -> Result<Vec<u8>> {
    // version 1: composition offsets are signed i32, supporting B-frame
    // scenarios where PTS < DTS (negative offset).
    full_box_result(b"ctts", 1, 0, |out| {
        let offsets: Vec<i32> = samples
            .iter()
            .map(|s| {
                let cts = i128::from(s.pts) - i128::from(s.dts);
                i32::try_from(cts)
                    .map_err(|_| Error::muxing("composition offset exceeds i32 range"))
            })
            .collect::<Result<_>>()?;
        let entries = grouped_counts(offsets);
        be_u32(out, entries.len() as u32);
        for (count, offset) in entries {
            be_u32(out, count);
            be_i32(out, offset);
        }
        Ok(())
    })
}

/// Composition to Decode Box. Helps players (notably QuickTime) correctly
/// handle B-frame CTS offsets, especially when some offsets are negative.
/// version 0 uses i32 fields.
fn cslg_box(samples: &[Mp4Sample]) -> Result<Vec<u8>> {
    let min = samples
        .iter()
        .map(|s| i128::from(s.pts) - i128::from(s.dts))
        .min()
        .unwrap_or(0);
    let max = samples
        .iter()
        .map(|s| i128::from(s.pts) - i128::from(s.dts))
        .max()
        .unwrap_or(0);
    let start = samples.iter().map(|s| i128::from(s.pts)).min().unwrap_or(0);
    let end = samples
        .iter()
        .map(|s| i128::from(s.pts) + i128::from(s.duration))
        .max()
        .unwrap_or(0);
    let values = [(-min).max(0), min, max, start, end];
    let wide = values.iter().any(|&v| i32::try_from(v).is_err());
    full_box_result(b"cslg", u8::from(wide), 0, |out| {
        for value in values {
            if wide {
                be_u64(
                    out,
                    i64::try_from(value)
                        .map_err(|_| Error::muxing("composition timeline exceeds i64"))?
                        as u64,
                );
            } else {
                be_i32(out, value as i32);
            }
        }
        Ok(())
    })
}

fn stss_box(samples: &[Mp4Sample]) -> Vec<u8> {
    full_box(b"stss", 0, 0, |out| {
        let key_indices: Vec<_> = samples
            .iter()
            .enumerate()
            .filter_map(|(index, sample)| sample.is_key.then_some((index + 1) as u32))
            .collect();
        be_u32(out, key_indices.len() as u32);
        for index in key_indices {
            be_u32(out, index);
        }
    })
}

/// One chunk's layout within a track. A chunk is a contiguous run of samples
/// in the mdat; grouping samples into ~0.5s chunks keeps stsc/stco compact
/// (one entry per chunk) instead of one entry per sample.
#[derive(Debug, Clone, Copy)]
struct ChunkMeta {
    /// Index into `track.samples()` of the first sample in this chunk.
    first_sample: usize,
    /// Number of samples in this chunk.
    sample_count: u32,
}

fn stsc_box(chunks: &[ChunkMeta]) -> Vec<u8> {
    // Compact encoding: merge runs of adjacent chunks that share the same
    // sample_count into a single stsc entry.
    let mut entries: Vec<(u32, u32, u32)> = Vec::new(); // (first_chunk, samples_per_chunk, desc_idx)
    for (index, chunk) in chunks.iter().enumerate() {
        if let Some((_, spc, _)) = entries.last_mut()
            && *spc == chunk.sample_count
        {
            continue;
        }
        entries.push(((index + 1) as u32, chunk.sample_count, 1));
    }
    full_box(b"stsc", 0, 0, |out| {
        be_u32(out, entries.len() as u32);
        for (first_chunk, spc, desc_idx) in entries {
            be_u32(out, first_chunk);
            be_u32(out, spc);
            be_u32(out, desc_idx);
        }
    })
}

fn stsz_box(samples: &[Mp4Sample]) -> Result<Vec<u8>> {
    full_box_result(b"stsz", 0, 0, |out| {
        be_u32(out, 0);
        be_u32(out, samples.len() as u32);
        for sample in samples {
            be_u32(out, fit_u32(sample.size(), "sample size")?);
        }
        Ok(())
    })
}

fn stco_box(samples: &[Mp4Sample], chunks: &[ChunkMeta]) -> Result<Vec<u8>> {
    let wide = chunks
        .iter()
        .any(|c| samples[c.first_sample].offset > u64::from(u32::MAX));
    full_box_result(if wide { b"co64" } else { b"stco" }, 0, 0, |out| {
        be_u32(out, fit_u32(chunks.len() as u64, "chunk count")?);
        for chunk in chunks {
            let offset = samples[chunk.first_sample].offset;
            if wide {
                be_u64(out, offset);
            } else {
                be_u32(out, fit_u32(offset, "chunk offset")?);
            }
        }
        Ok(())
    })
}

fn grouped_counts<I, T>(values: I) -> Vec<(u32, T)>
where
    I: IntoIterator<Item = T>,
    T: PartialEq + Copy,
{
    let mut entries: Vec<(u32, T)> = Vec::new();
    for value in values {
        if let Some((count, last)) = entries.last_mut()
            && *last == value
        {
            *count += 1;
            continue;
        }
        entries.push((1, value));
    }
    entries
}

fn track_timescale(track: &Mp4Track) -> u32 {
    match track {
        Mp4Track::Video { timescale, .. } | Mp4Track::Audio { timescale, .. } => *timescale,
    }
}

/// Splits a track's samples into chunks of roughly 0.5 seconds each. Sample
/// durations are in the track's own timescale. The final chunk absorbs any
/// remaining samples even if shorter than the threshold.
fn split_chunks(track: &Mp4Track) -> Vec<ChunkMeta> {
    let samples = track.samples();
    if samples.is_empty() {
        return Vec::new();
    }
    let track_ts = track_timescale(track);
    // 0.5s in the track's timescale. Use saturating mul to avoid overflow on
    // unusually large timescales; fall back to a single chunk if so.
    let threshold = (track_ts as u64).saturating_mul(500) / 1000;

    let mut chunks = Vec::new();
    let mut current = ChunkMeta {
        first_sample: 0,
        sample_count: 0,
    };
    let mut current_duration: u64 = 0;
    for (index, sample) in samples.iter().enumerate() {
        if current_duration >= threshold && current.sample_count > 0 {
            chunks.push(current);
            current = ChunkMeta {
                first_sample: index,
                sample_count: 0,
            };
            current_duration = 0;
        }
        current.sample_count += 1;
        current_duration += u64::from(sample.duration);
    }
    if current.sample_count > 0 {
        chunks.push(current);
    }
    chunks
}

fn rescale(value: u64, from: u32, to: u32) -> Result<u64> {
    if from == 0 {
        return Err(Error::muxing("zero timescale"));
    }
    u64::try_from(u128::from(value) * u128::from(to) / u128::from(from))
        .map_err(|_| Error::muxing("duration rescale overflow"))
}

fn fit_u32(value: u64, field: &str) -> Result<u32> {
    u32::try_from(value).map_err(|_| Error::muxing(format!("{field} exceeds u32")))
}

fn boxed(kind: &[u8; 4], write: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut payload = Vec::new();
    write(&mut payload);
    let mut out = Vec::with_capacity(8 + payload.len());
    write_box_header(&mut out, kind, 8 + payload.len())
        .expect("box generated by boxed is too large");
    out.extend_from_slice(&payload);
    out
}

fn boxed_result(kind: &[u8; 4], write: impl FnOnce(&mut Vec<u8>) -> Result<()>) -> Result<Vec<u8>> {
    let mut payload = Vec::new();
    write(&mut payload)?;
    let mut out = Vec::with_capacity(8 + payload.len());
    write_box_header(&mut out, kind, 8 + payload.len())?;
    out.extend_from_slice(&payload);
    Ok(out)
}

fn full_box(kind: &[u8; 4], version: u8, flags: u32, write: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    boxed(kind, |out| {
        out.push(version);
        be_u24(out, flags);
        write(out);
    })
}

fn full_box_result(
    kind: &[u8; 4],
    version: u8,
    flags: u32,
    write: impl FnOnce(&mut Vec<u8>) -> Result<()>,
) -> Result<Vec<u8>> {
    boxed_result(kind, |out| {
        out.push(version);
        be_u24(out, flags);
        write(out)
    })
}

fn write_box_header(out: &mut Vec<u8>, kind: &[u8; 4], size: usize) -> Result<()> {
    if let Ok(small) = u32::try_from(size) {
        be_u32(out, small);
        out.extend_from_slice(kind);
    } else {
        be_u32(out, 1);
        out.extend_from_slice(kind);
        be_u64(
            out,
            (size as u64)
                .checked_add(8)
                .ok_or_else(|| Error::muxing("box size overflow"))?,
        );
    }
    Ok(())
}

fn unity_matrix(out: &mut Vec<u8>) {
    be_u32(out, 0x0001_0000);
    be_u32(out, 0);
    be_u32(out, 0);
    be_u32(out, 0);
    be_u32(out, 0x0001_0000);
    be_u32(out, 0);
    be_u32(out, 0);
    be_u32(out, 0);
    be_u32(out, 0x4000_0000);
}

fn be_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn be_u24(out: &mut Vec<u8>, value: u32) {
    out.push(((value >> 16) & 0xff) as u8);
    out.push(((value >> 8) & 0xff) as u8);
    out.push((value & 0xff) as u8);
}

fn be_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn be_i32(out: &mut Vec<u8>, value: i32) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn be_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_be_bytes());
}

#[derive(Debug, Clone)]
pub(crate) struct FragmentedTrack {
    pub track_id: u32,
    pub timescale: u32,
    pub kind: FragmentedTrackKind,
}

#[derive(Debug, Clone)]
pub(crate) enum FragmentedTrackKind {
    Video {
        width: u16,
        height: u16,
        codec: VideoCodec,
    },
    Audio {
        sample_rate: u32,
        channel_count: u8,
        audio_specific_config: Vec<u8>,
    },
}

impl FragmentedTrack {
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn into_classic(self, samples: Vec<Mp4Sample>) -> Mp4Track {
        match self.kind {
            FragmentedTrackKind::Video {
                width,
                height,
                codec,
            } => Mp4Track::Video {
                samples,
                timescale: self.timescale,
                width,
                height,
                codec,
            },
            FragmentedTrackKind::Audio {
                sample_rate,
                channel_count,
                audio_specific_config,
            } => Mp4Track::Audio {
                samples,
                timescale: self.timescale,
                sample_rate,
                channel_count,
                audio_specific_config,
            },
        }
    }

    pub(crate) fn avc_video(track_id: u32, sps: &[u8], pps: &[u8]) -> Result<Self> {
        let info = avc::parse_sps(sps)?;
        let avcc = avc::avcc(sps, pps)?;
        Ok(Self {
            track_id,
            timescale: 90_000,
            kind: FragmentedTrackKind::Video {
                width: info.width,
                height: info.height,
                codec: VideoCodec::Avc { avcc },
            },
        })
    }

    pub(crate) fn hevc_video(track_id: u32, vps: &[u8], sps: &[u8], pps: &[u8]) -> Result<Self> {
        use crate::codecs::hevc;
        let info = hevc::parse_sps(sps)?;
        let hvcc = hevc::hvcc(vps, sps, pps)?;
        Ok(Self {
            track_id,
            timescale: 90_000,
            kind: FragmentedTrackKind::Video {
                width: info.width,
                height: info.height,
                codec: VideoCodec::Hevc { hvcc },
            },
        })
    }

    pub(crate) fn audio(
        track_id: u32,
        sample_rate: u32,
        channel_count: u8,
        audio_specific_config: Vec<u8>,
    ) -> Self {
        Self {
            track_id,
            timescale: sample_rate,
            kind: FragmentedTrackKind::Audio {
                sample_rate,
                channel_count,
                audio_specific_config,
            },
        }
    }
}

pub(crate) struct FragmentedMp4Muxer {
    tracks: Vec<FragmentedTrack>,
    next_sequence: u32,
}

impl FragmentedMp4Muxer {
    pub(crate) fn new(tracks: Vec<FragmentedTrack>) -> Self {
        Self {
            tracks,
            next_sequence: 1,
        }
    }

    /// Resumes a fragmented muxer at a specific sequence number. Used by
    /// the resume path to continue appending fragments with the correct
    /// `mfhd` sequence after an interrupted run.
    pub(crate) fn new_with_sequence(tracks: Vec<FragmentedTrack>, next_sequence: u32) -> Self {
        Self {
            tracks,
            next_sequence,
        }
    }

    /// Returns the next fragment's `mfhd` sequence number. Used by the
    /// streaming pipeline to snapshot the resume checkpoint after each
    /// fragment write.
    pub(crate) fn next_sequence(&self) -> u32 {
        self.next_sequence
    }

    pub(crate) fn write_header(&self) -> Result<Vec<u8>> {
        let ftyp = fragmented_ftyp_box();
        // Fragmented moov uses duration=0; the real timeline is carried by
        // per-fragment tfdt/trun. Writing a non-zero duration here makes some
        // players (QuickTime) treat it as a hard cap and stall on seek.
        let moov = fragmented_moov_box(&self.tracks)?;
        let mut out = Vec::with_capacity(ftyp.len() + moov.len());
        out.extend_from_slice(&ftyp);
        out.extend_from_slice(&moov);
        Ok(out)
    }

    /// Writes one `styp` + `moof` + `mdat` fragment.
    ///
    /// `samples_per_track[i]` corresponds to `self.tracks[i]`. Tracks with no
    /// samples in this fragment should pass an empty slice.
    pub(crate) fn write_fragment(
        &mut self,
        samples_per_track: &[Vec<Mp4Sample>],
    ) -> Result<Vec<u8>> {
        if samples_per_track.len() != self.tracks.len() {
            return Err(Error::invalid(
                "samples_per_track length must match tracks length",
            ));
        }

        let styp = styp_box();
        let moof = self.fragment_moof_box(samples_per_track)?;
        let mdat_payload_size = samples_per_track
            .iter()
            .flat_map(|samples| samples.iter())
            .try_fold(0_u64, |acc, sample| {
                acc.checked_add(sample.data.len() as u64)
                    .ok_or_else(|| Error::muxing("fragment media data is too large"))
            })?;
        if mdat_payload_size
            .checked_add(8)
            .is_none_or(|size| size > u64::from(u32::MAX))
        {
            return Err(Error::unsupported(
                "fragments larger than 4 GiB are out of Phase 3 scope",
            ));
        }

        let capacity = (styp.len() as u64)
            .checked_add(moof.len() as u64)
            .and_then(|size| size.checked_add(8))
            .and_then(|size| size.checked_add(mdat_payload_size))
            .and_then(|size| usize::try_from(size).ok())
            .ok_or_else(|| Error::muxing("fragment exceeds address space"))?;
        let mut out = Vec::new();
        out.try_reserve_exact(capacity)
            .map_err(|_| Error::muxing("fragment allocation failed"))?;
        out.extend_from_slice(&styp);
        out.extend_from_slice(&moof);
        write_box_header(&mut out, b"mdat", 8 + mdat_payload_size as usize)?;
        for samples in samples_per_track {
            for sample in samples {
                out.extend_from_slice(&sample.data);
            }
        }

        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or_else(|| Error::muxing("fragment sequence number overflowed"))?;
        Ok(out)
    }

    fn fragment_moof_box(&self, samples_per_track: &[Vec<Mp4Sample>]) -> Result<Vec<u8>> {
        let sequence = self.next_sequence;
        // Pre-compute moof size and per-track data_offsets so the trun's
        // data_offset field can point at the correct byte in the mdat payload.
        let mut moof_payload_size: usize = 16; // mfhd box
        for (index, _track) in self.tracks.iter().enumerate() {
            let samples = &samples_per_track[index];
            if samples.is_empty() {
                continue;
            }
            // tfhd (16) + tfdt (20) + trun (20 + 16 * sample_count) + traf header (8)
            fit_u32(samples.len() as u64, "fragment sample count")?;
            moof_payload_size = samples
                .len()
                .checked_mul(16)
                .and_then(|n| n.checked_add(64))
                .and_then(|n| moof_payload_size.checked_add(n))
                .ok_or_else(|| Error::muxing("moof size overflow"))?;
        }
        let moof_size = moof_payload_size
            .checked_add(8)
            .ok_or_else(|| Error::muxing("moof size overflow"))?;
        let mdat_header_size = 8;

        let mut data_offset = moof_size
            .checked_add(mdat_header_size)
            .ok_or_else(|| Error::muxing("fragment offset overflow"))?;
        let moof = boxed_result(b"moof", |out| {
            out.extend_from_slice(&mfhd_box(sequence)?);
            for (index, track) in self.tracks.iter().enumerate() {
                let samples = &samples_per_track[index];
                if samples.is_empty() {
                    continue;
                }
                let signed_offset = i32::try_from(data_offset)
                    .map_err(|_| Error::muxing("fragment data offset exceeds i32"))?;
                out.extend_from_slice(&traf_box(track, samples, signed_offset as u32)?);
                data_offset = samples.iter().try_fold(data_offset, |offset, sample| {
                    offset
                        .checked_add(sample.data.len())
                        .ok_or_else(|| Error::muxing("fragment offset overflow"))
                })?;
            }
            Ok(())
        })?;
        // Sanity check: the generated moof matches our pre-computed size.
        debug_assert_eq!(moof.len(), moof_size);
        Ok(moof)
    }
}

fn styp_box() -> Vec<u8> {
    boxed(b"styp", |out| {
        out.extend_from_slice(b"msdh");
        be_u32(out, 0);
        out.extend_from_slice(b"msdh");
        out.extend_from_slice(b"iso2");
    })
}

fn fragmented_moov_box(tracks: &[FragmentedTrack]) -> Result<Vec<u8>> {
    let movie_timescale = 1000_u32;
    boxed_result(b"moov", |out| {
        out.extend_from_slice(&mvhd_box(movie_timescale, 0, tracks.len())?);
        for track in tracks {
            out.extend_from_slice(&fragmented_trak_box(track)?);
        }
        out.extend_from_slice(&mvex_box(tracks)?);
        Ok(())
    })
}

fn fragmented_trak_box(track: &FragmentedTrack) -> Result<Vec<u8>> {
    boxed_result(b"trak", |out| {
        out.extend_from_slice(&fragmented_tkhd_box(track)?);
        out.extend_from_slice(&fragmented_mdia_box(track)?);
        Ok(())
    })
}

fn fragmented_tkhd_box(track: &FragmentedTrack) -> Result<Vec<u8>> {
    full_box_result(b"tkhd", 0, 0x000007, |out| {
        be_u32(out, 0);
        be_u32(out, 0);
        be_u32(out, track.track_id);
        be_u32(out, 0);
        be_u32(out, 0);
        be_u32(out, 0);
        be_u32(out, 0);
        be_u16(out, 0);
        be_u16(out, 0);
        be_u16(
            out,
            if matches!(track.kind, FragmentedTrackKind::Audio { .. }) {
                0x0100
            } else {
                0
            },
        );
        be_u16(out, 0);
        unity_matrix(out);
        match &track.kind {
            FragmentedTrackKind::Video { width, height, .. } => {
                be_u32(out, u32::from(*width) << 16);
                be_u32(out, u32::from(*height) << 16);
            }
            FragmentedTrackKind::Audio { .. } => {
                be_u32(out, 0);
                be_u32(out, 0);
            }
        }
        Ok(())
    })
}

fn fragmented_mdia_box(track: &FragmentedTrack) -> Result<Vec<u8>> {
    boxed_result(b"mdia", |out| {
        out.extend_from_slice(&mdhd_box(track.timescale, 0)?);
        out.extend_from_slice(&fragmented_hdlr_box(track));
        out.extend_from_slice(&fragmented_minf_box(track)?);
        Ok(())
    })
}

fn fragmented_hdlr_box(track: &FragmentedTrack) -> Vec<u8> {
    full_box(b"hdlr", 0, 0, |out| {
        be_u32(out, 0);
        match &track.kind {
            FragmentedTrackKind::Video { .. } => out.extend_from_slice(b"vide"),
            FragmentedTrackKind::Audio { .. } => out.extend_from_slice(b"soun"),
        }
        be_u32(out, 0);
        be_u32(out, 0);
        be_u32(out, 0);
        match &track.kind {
            FragmentedTrackKind::Video { .. } => out.extend_from_slice(b"VideoHandler\0"),
            FragmentedTrackKind::Audio { .. } => out.extend_from_slice(b"SoundHandler\0"),
        }
    })
}

fn fragmented_minf_box(track: &FragmentedTrack) -> Result<Vec<u8>> {
    boxed_result(b"minf", |out| {
        match &track.kind {
            FragmentedTrackKind::Video { .. } => out.extend_from_slice(&vmhd_box()),
            FragmentedTrackKind::Audio { .. } => out.extend_from_slice(&smhd_box()),
        }
        out.extend_from_slice(&dinf_box());
        out.extend_from_slice(&fragmented_stbl_box(track)?);
        Ok(())
    })
}

fn fragmented_stbl_box(track: &FragmentedTrack) -> Result<Vec<u8>> {
    boxed_result(b"stbl", |out| {
        out.extend_from_slice(&fragmented_stsd_box(track)?);
        // Empty sample tables — the real tables live in moof/trun.
        out.extend_from_slice(&full_box(b"stts", 0, 0, |o| be_u32(o, 0)));
        out.extend_from_slice(&full_box(b"stsc", 0, 0, |o| be_u32(o, 0)));
        out.extend_from_slice(&full_box(b"stsz", 0, 0, |o| {
            be_u32(o, 0);
            be_u32(o, 0);
        }));
        out.extend_from_slice(&full_box(b"stco", 0, 0, |o| be_u32(o, 0)));
        Ok(())
    })
}

fn fragmented_stsd_box(track: &FragmentedTrack) -> Result<Vec<u8>> {
    full_box_result(b"stsd", 0, 0, |out| {
        be_u32(out, 1);
        match &track.kind {
            FragmentedTrackKind::Video {
                width,
                height,
                codec,
            } => match codec {
                VideoCodec::Avc { avcc } => {
                    out.extend_from_slice(&avc1_entry(*width, *height, avcc)?)
                }
                VideoCodec::Hevc { hvcc } => {
                    out.extend_from_slice(&hvc1_entry(*width, *height, hvcc)?)
                }
            },
            FragmentedTrackKind::Audio {
                sample_rate,
                channel_count,
                audio_specific_config,
            } => out.extend_from_slice(&mp4a_entry(
                *sample_rate,
                *channel_count,
                audio_specific_config,
            )?),
        }
        Ok(())
    })
}

fn mvex_box(tracks: &[FragmentedTrack]) -> Result<Vec<u8>> {
    boxed_result(b"mvex", |out| {
        for track in tracks {
            out.extend_from_slice(&trex_box(track)?);
        }
        Ok(())
    })
}

fn trex_box(track: &FragmentedTrack) -> Result<Vec<u8>> {
    full_box_result(b"trex", 0, 0, |out| {
        be_u32(out, track.track_id);
        be_u32(out, 1); // default_sample_description_index
        be_u32(out, 0); // default_sample_duration
        be_u32(out, 0); // default_sample_size
        be_u32(out, 0); // default_sample_flags
        Ok(())
    })
}

/// One entry in a Track Fragment Random Access table. Points at the first
/// sync sample of a fragment so players can seek without scanning moof boxes.
#[derive(Debug, Clone)]
pub(crate) struct TfraEntry {
    /// Presentation time of the first sample in this fragment, in the track's
    /// own timescale.
    pub time: u64,
    /// Absolute byte offset of the fragment's `moof` box from the start of
    /// the file.
    pub moof_offset: u64,
    pub traf_number: u32,
    pub trun_number: u32,
    pub sample_number: u32,
}

/// Movie Fragment Random Access Box. Written at the very end of the file so
/// players can locate sync samples by seeking from EOF. Contains one `tfra`
/// per track plus a trailing `mfro` whose size field equals the full `mfra`
/// length (the size is computed up-front so no post-write patching is needed).
pub(crate) fn mfra_box(
    tracks: &[FragmentedTrack],
    entries_per_track: &[Vec<TfraEntry>],
) -> Result<Vec<u8>> {
    let mut tfras = Vec::with_capacity(tracks.len());
    let mut total_size: usize = 8; // mfra box header
    for (index, track) in tracks.iter().enumerate() {
        let tfra = tfra_box(track.track_id, &entries_per_track[index])?;
        total_size += tfra.len();
        tfras.push(tfra);
    }
    total_size += 16; // mfro: 8 header + 4 version/flags + 4 size

    boxed_result(b"mfra", |out| {
        for tfra in &tfras {
            out.extend_from_slice(tfra);
        }
        out.extend_from_slice(&mfro_box(fit_u32(total_size as u64, "mfra size")?));
        Ok(())
    })
}

fn tfra_box(track_id: u32, entries: &[TfraEntry]) -> Result<Vec<u8>> {
    full_box_result(b"tfra", 1, 0, |out| {
        be_u32(out, track_id);
        // 26 reserved bits + 2-bit traf length + 2-bit trun length + 2-bit sample length.
        // 0x3F → all three fields are 32-bit.
        be_u32(out, 0x0000_003f);
        be_u32(out, fit_u32(entries.len() as u64, "tfra entry count")?);
        for entry in entries {
            be_u64(out, entry.time);
            be_u64(out, entry.moof_offset);
            be_u32(out, entry.traf_number);
            be_u32(out, entry.trun_number);
            be_u32(out, entry.sample_number);
        }
        Ok(())
    })
}

fn mfro_box(size: u32) -> Vec<u8> {
    full_box(b"mfro", 0, 0, |out| {
        be_u32(out, size);
    })
}

fn mfhd_box(sequence_number: u32) -> Result<Vec<u8>> {
    full_box_result(b"mfhd", 0, 0, |out| {
        be_u32(out, sequence_number);
        Ok(())
    })
}

fn traf_box(track: &FragmentedTrack, samples: &[Mp4Sample], data_offset: u32) -> Result<Vec<u8>> {
    let base_decode_time = samples.first().map(|s| s.dts).unwrap_or(0);
    boxed_result(b"traf", |out| {
        out.extend_from_slice(&tfhd_box(track)?);
        out.extend_from_slice(&tfdt_box(base_decode_time)?);
        out.extend_from_slice(&trun_box(samples, data_offset)?);
        Ok(())
    })
}

fn tfhd_box(track: &FragmentedTrack) -> Result<Vec<u8>> {
    // default-base-is-moof (0x020000), sample-description-index-present absent
    full_box_result(b"tfhd", 0, 0x020_000, |out| {
        be_u32(out, track.track_id);
        Ok(())
    })
}

fn tfdt_box(base_decode_time: u64) -> Result<Vec<u8>> {
    full_box_result(b"tfdt", 1, 0, |out| {
        be_u64(out, base_decode_time);
        Ok(())
    })
}

fn trun_box(samples: &[Mp4Sample], data_offset: u32) -> Result<Vec<u8>> {
    // data-offset-present (0x000001), sample-duration-present (0x000100),
    // sample-size-present (0x000200), sample-flags-present (0x000400),
    // sample-composition-time-offset-present (0x000800); version=1 for signed cts
    let flags: u32 = 0x000_f01;
    full_box_result(b"trun", 1, flags, |out| {
        be_u32(out, samples.len() as u32);
        be_u32(out, data_offset);
        for sample in samples {
            be_u32(out, sample.duration);
            be_u32(
                out,
                fit_u32(sample.data.len() as u64, "fragment sample size")?,
            );
            // Per ISO/IEC 14496-12 sample_flags bit layout:
            //   bits 25-24: sample_depends_on (2=independent/I-frame, 1=depends/P-B)
            //   bit 16: sample_is_non_sync_sample (0 for SAP, 1 for non-sync)
            // Key frame: 0x02000000 (independent + sync)
            // Delta frame: 0x01010000 (depends + non-sync)
            let sample_flags = if sample.is_key {
                0x0200_0000
            } else {
                0x0101_0000
            };
            be_u32(out, sample_flags);
            // composition offset (signed, i32 reinterpreted as u32 for wire format)
            let cts = i128::from(sample.pts) - i128::from(sample.dts);
            let cts = i32::try_from(cts)
                .map_err(|_| Error::muxing("fragment composition offset exceeds i32"))?;
            be_u32(out, cts as u32);
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_ftyp_box() {
        // ftyp_box inspects track codecs to choose compatible brands; an
        // empty slice yields just isom + mp41.
        let ftyp = ftyp_box(&[]);
        assert_eq!(&ftyp[4..8], b"ftyp");
        assert_eq!(&ftyp[8..12], b"isom");
    }

    #[test]
    fn groups_stts_entries() {
        let samples = vec![
            sample(0, 0, 100),
            sample(100, 100, 100),
            sample(200, 200, 120),
        ];
        let stts = stts_box(&samples);
        assert_eq!(&stts[4..8], b"stts");
        assert_eq!(u32::from_be_bytes(stts[12..16].try_into().unwrap()), 2);
    }

    #[test]
    fn fragmented_muxer_emits_init_and_fragment() {
        // Minimal fake "AVC" config: a tiny SPS NAL (type 7) + PPS NAL (type 8).
        // SPS bytes: [nal_header=0x67, profile=1, compat=2, level=3, then a minimal
        // but parseable SPS body] — wide/height are read via Exp-Golomb.
        // We avoid parse_sps here by constructing FragmentedTrack via the enum
        // directly so this test stays focused on the muxer box layout.
        let track = FragmentedTrack {
            track_id: 1,
            timescale: 90_000,
            kind: FragmentedTrackKind::Video {
                width: 320,
                height: 180,
                codec: VideoCodec::Avc { avcc: vec![0x01] },
            },
        };
        let mut muxer = FragmentedMp4Muxer::new(vec![track]);

        let header = muxer.write_header().unwrap();
        // ftyp + moov
        assert_eq!(&header[4..8], b"ftyp");
        let moov_pos = header
            .windows(4)
            .position(|w| w == b"moov")
            .expect("moov box present");
        assert_eq!(&header[moov_pos..moov_pos + 4], b"moov");
        // mvex present
        assert!(header.windows(4).any(|w| w == b"mvex"));

        let samples = vec![sample(0, 0, 1000), sample(1000, 1000, 1000)];
        let fragment = muxer.write_fragment(&[samples]).unwrap();
        // styp + moof + mdat
        assert_eq!(&fragment[4..8], b"styp");
        assert!(fragment.windows(4).any(|w| w == b"moof"));
        assert!(fragment.windows(4).any(|w| w == b"mdat"));
        // second fragment gets sequence number 2
        let _fragment2 = muxer.write_fragment(&[vec![sample(0, 0, 1000)]]).unwrap();
    }

    fn audio(samples: Vec<Mp4Sample>, timescale: u32) -> Mp4Track {
        Mp4Track::Audio {
            samples,
            timescale,
            sample_rate: 48000,
            channel_count: 2,
            audio_specific_config: vec![0x11, 0x90],
        }
    }

    #[test]
    fn wide_offsets_mdat_and_duration_boundaries() {
        for offset in [
            u64::from(u32::MAX) - 1,
            u64::from(u32::MAX),
            u64::from(u32::MAX) + 1,
        ] {
            let mut s = sample(0, 0, 1);
            s.offset = offset;
            let out = stco_box(
                &[s],
                &[ChunkMeta {
                    first_sample: 0,
                    sample_count: 1,
                }],
            )
            .unwrap();
            assert_eq!(
                &out[4..8],
                if offset > u64::from(u32::MAX) {
                    b"co64"
                } else {
                    b"stco"
                }
            );
        }
        for payload in [
            u64::from(u32::MAX) - 9,
            u64::from(u32::MAX) - 8,
            u64::from(u32::MAX) - 7,
        ] {
            assert_eq!(
                mdat_header(payload).unwrap().len(),
                if payload + 8 > u64::from(u32::MAX) {
                    16
                } else {
                    8
                }
            );
        }
        assert!(mdat_header(u64::MAX).is_err());
        let track = audio(vec![sample(0, 0, 1)], 48000);
        for duration in [
            u64::from(u32::MAX) - 1,
            u64::from(u32::MAX),
            u64::from(u32::MAX) + 1,
        ] {
            let version = u8::from(duration > u64::from(u32::MAX));
            assert_eq!(mvhd_box(1000, duration, 1).unwrap()[8], version);
            assert_eq!(tkhd_box(&track, 1, duration).unwrap()[8], version);
            assert_eq!(mdhd_box(48000, duration).unwrap()[8], version);
            let edits = edts_box(duration, duration).unwrap();
            assert_eq!(edits[16], version);
        }
        assert!(rescale(u64::MAX, 1, 1000).is_err());
        assert_eq!(rescale(u64::MAX, 1000, 1000).unwrap(), u64::MAX);
    }

    #[test]
    fn layout_recalculates_faststart_after_co64_promotion() {
        let mut large = sample(0, 0, 48000);
        large.data.clear();
        large.source = Some((0, u32::MAX));
        let mut last = sample(48000, 48000, 48000);
        last.source = Some((0, 1));
        last.data.clear();
        let mut metadata = Vec::new();
        let (size, _) = Mp4Muxer::new(vec![audio(vec![large, last], 48000)])
            .write_to(&mut metadata, &|| Ok(()), |_, _| Ok(()))
            .unwrap();
        let pos = metadata.windows(4).position(|w| w == b"co64").unwrap();
        let first = u64::from_be_bytes(metadata[pos + 12..pos + 20].try_into().unwrap());
        let second = u64::from_be_bytes(metadata[pos + 20..pos + 28].try_into().unwrap());
        assert_eq!(first, metadata.len() as u64);
        assert_eq!(second, first + u64::from(u32::MAX));
        assert_eq!(size, first + u64::from(u32::MAX) + 1);
        assert_eq!(
            u32::from_be_bytes(
                metadata[metadata.len() - 16..metadata.len() - 12]
                    .try_into()
                    .unwrap()
            ),
            1
        );
    }

    #[test]
    fn co64_growth_can_promote_another_tracks_offsets() {
        let file_sample = |dts, size| {
            let mut s = sample(dts, dts, 48000);
            s.data.clear();
            s.source = Some((0, size));
            s
        };
        let mut tracks = vec![
            audio(vec![file_sample(0, 1), file_sample(96000, 16)], 48000),
            audio(vec![file_sample(48000, 8)], 48000),
        ];
        let chunks: Vec<_> = tracks.iter().map(split_chunks).collect();
        let base = (ftyp_box(&tracks).len() + moov_box(&tracks, &chunks).unwrap().len() + 8) as u64;
        tracks[0].samples_mut()[0].source = Some((0, (u64::from(u32::MAX) - base - 4) as u32));
        let mut bytes = Vec::new();
        Mp4Muxer::new(tracks)
            .write_to(&mut bytes, &|| Ok(()), |_, _| Ok(()))
            .unwrap();
        let tables: Vec<_> = bytes
            .windows(4)
            .enumerate()
            .filter_map(|(i, w)| (w == b"co64").then_some(i))
            .collect();
        assert_eq!(tables.len(), 2);
        let second_track_offset =
            u64::from_be_bytes(bytes[tables[1] + 12..tables[1] + 20].try_into().unwrap());
        assert_eq!(second_track_offset, u64::from(u32::MAX) + 8);
    }

    #[test]
    fn chunks_compare_track_time_rather_than_raw_dts() {
        let a = audio(
            vec![sample(0, 0, 90000), sample(90000, 90000, 90000)],
            90000,
        );
        let b = audio(
            vec![sample(0, 0, 48000), sample(48000, 48000, 48000)],
            48000,
        );
        let mut order = Vec::new();
        Mp4Muxer::new(vec![a, b])
            .write_to(&mut Vec::new(), &|| Ok(()), |sample, _| {
                order.push(sample.dts);
                Ok(())
            })
            .unwrap();
        assert_eq!(order, [0, 0, 90000, 48000]);
    }

    #[test]
    fn invalid_sample_timeline_is_rejected_and_cslg_can_be_wide() {
        let s = sample(u64::MAX, u64::MAX, 1);
        assert!(
            Mp4Muxer::new(vec![audio(vec![s], 48000)])
                .write_checked(&|| Ok(()))
                .is_err()
        );
        let s = sample(u64::from(u32::MAX), u64::from(u32::MAX) + 1, 1);
        assert_eq!(cslg_box(&[s]).unwrap()[8], 1);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn file_copy_checks_cancel_between_blocks_and_rejects_short_source() {
        use std::cell::Cell;
        use std::fs::File;
        use std::io::Write;
        let folder = std::env::temp_dir().join(format!("hls-copy-test-{}", std::process::id()));
        std::fs::create_dir(&folder).unwrap();
        let mut source = File::create(folder.join("source")).unwrap();
        source.write_all(&[1]).unwrap();
        source.set_len(3 * 1024 * 1024).unwrap();
        drop(source);
        let track = || {
            let mut s = sample(0, 0, 1024);
            s.data.clear();
            s.source = Some((0, 3 * 1024 * 1024));
            audio(vec![s], 48000)
        };
        let mut input = File::open(folder.join("source")).unwrap();
        let mut output = File::create(folder.join("output")).unwrap();
        let checks = Cell::new(0);
        let result = Mp4Muxer::new(vec![track()]).write_file(&mut input, &mut output, &|| {
            checks.set(checks.get() + 1);
            // Observe the first copied block deterministically, independent of speed.
            if std::fs::metadata(folder.join("output"))?.len() > 1024 * 1024 {
                Err(Error::Cancelled)
            } else {
                Ok(())
            }
        });
        assert!(matches!(result, Err(Error::Cancelled)));
        assert!(checks.get() > 1);
        assert!(output.metadata().unwrap().len() < 3 * 1024 * 1024);
        drop(input);
        drop(output);
        File::create(folder.join("source"))
            .unwrap()
            .set_len(1)
            .unwrap();
        let mut input = File::open(folder.join("source")).unwrap();
        let mut output = File::create(folder.join("output")).unwrap();
        assert!(
            Mp4Muxer::new(vec![track()])
                .write_file(&mut input, &mut output, &|| Ok(()))
                .is_err()
        );
        drop(input);
        drop(output);
        std::fs::remove_dir_all(folder).unwrap();
    }

    /// Sparse synthetic media validates >4 GiB tables/seeking, not decoding.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    #[ignore = "manual sparse >4 GiB structural/seek check; requires ffprobe"]
    fn sparse_large_mp4_ffprobe_seek() {
        use std::io::{Seek, SeekFrom};
        let path = std::env::temp_dir().join(format!("hls-large-{}.mp4", std::process::id()));
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _cleanup = Cleanup(path.clone());
        let samples = (0..4200)
            .map(|i| {
                let mut s = sample(i * 1024, i * 1024, 1024);
                s.data.clear();
                s.source = Some((0, 1024 * 1024));
                s
            })
            .collect();
        let mut file = std::fs::File::create(&path).unwrap();
        let (size, _) = Mp4Muxer::new(vec![audio(samples, 48000)])
            .write_to(&mut file, &|| Ok(()), |s, out| {
                out.seek(SeekFrom::Current(s.size() as i64))?;
                Ok(())
            })
            .unwrap();
        file.set_len(size).unwrap();
        drop(file);
        assert!(size > u64::from(u32::MAX));
        let output = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "quiet",
                "-analyzeduration",
                "0",
                "-probesize",
                "32",
                "-select_streams",
                "a:0",
                "-read_intervals",
                "89%+0.1",
                "-show_packets",
                "-show_entries",
                "packet=pos,size,dts",
                "-of",
                "json",
            ])
            .arg(&path)
            .output()
            .expect("ffprobe must be installed for this manual test");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let packets = json["packets"].as_array().unwrap();
        assert!(!packets.is_empty());
        let position: u64 = packets[0]["pos"].as_str().unwrap().parse().unwrap();
        assert!(
            position > u64::from(u32::MAX),
            "seek did not reach a 64-bit media offset"
        );
        assert_eq!(packets[0]["size"].as_str().unwrap(), "1048576");
        println!("FFprobe seek verified: file_bytes={size}, packet_offset={position}");
    }
    fn sample(dts: u64, pts: u64, duration: u32) -> Mp4Sample {
        Mp4Sample {
            data: vec![1, 2, 3],
            source: None,
            dts,
            pts,
            duration,
            is_key: true,
            offset: 0,
        }
    }
}

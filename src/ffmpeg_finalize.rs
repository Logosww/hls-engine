//! Optional ffmpeg-backed finalization for `StreamingMp4`.
//!
//! When the `ffmpeg-finalize` cargo feature is enabled, [`remux_blocking`] uses
//! ffmpeg (via `ffmpeg-next`) to convert the stage-1 fragmented MP4 temp file
//! into a standard non-fragmented MP4 (`ftyp` + `moov` + `mdat`, faststart).
//! This is a packet-copy remux — no decoding or encoding — and serves as an
//! alternative to the crate's self-contained [`defragment_fmp4_to_mp4`][crate::transmux]
//! path.
//!
//! `movflags=faststart` is passed via `write_header_with` so ffmpeg performs a
//! second pass after `write_trailer` to relocate `moov` ahead of `mdat`,
//! producing a layout suitable for progressive/networked playback.
//!
//! ffmpeg runs synchronously, so the work is moved to a blocking task via
//! [`tokio::task::spawn_blocking`].
//!
//! [`remux_blocking`]: remux_to_mp4

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ffmpeg_next as ffmpeg;

use crate::error::{Error, Result};

/// Remuxes the stage-1 fragmented MP4 (`temp_path`) into a standard MP4 at
/// `output` using ffmpeg. Packet-copy only (no transcode).
///
/// `segment_count` is forwarded into the returned [`TransmuxReport`] (ffmpeg
/// does not know the original HLS segment count).
pub(crate) fn remux_blocking(
    input: &Path,
    output: &PathBuf,
    check: &dyn Fn() -> Result<()>,
) -> Result<i64> {
    check()?;
    ffmpeg::init().map_err(|e| Error::muxing(format!("ffmpeg init: {e}")))?;

    let mut ictx = ffmpeg::format::input(input)
        .map_err(|e| Error::muxing(format!("ffmpeg open input {:?}: {e}", input)))?;
    let mut octx = ffmpeg::format::output(output)
        .map_err(|e| Error::muxing(format!("ffmpeg open output {:?}: {e}", output)))?;

    // Map input stream index -> output stream index. We add one output stream
    // per input stream with codec=None and copy codec parameters verbatim
    // (stream copy / remux, no decode/encode). This preserves all tracks.
    let mut stream_map: HashMap<usize, usize> = HashMap::new();
    for stream in ictx.streams() {
        check()?;
        let codec = ffmpeg::encoder::find(ffmpeg::codec::Id::None);
        let mut ostream = octx
            .add_stream(codec)
            .map_err(|e| Error::muxing(format!("ffmpeg add_stream: {e}")))?;
        ostream.set_parameters(stream.parameters());
        stream_map.insert(stream.index(), ostream.index());
    }

    // Request faststart (moov before mdat) via the mov muxer private option.
    // Without this, ffmpeg writes moov at EOF — fine for local playback but
    // poor for progressive/networked consumption. `write_header_with` passes
    // the dictionary to avformat_write_header, which forwards unknown keys to
    // the muxer's private options (movflags lives on the mov muxer). The
    // returned dictionary holds a borrow on `octx`; drop it before we mutably
    // borrow `octx` again for write_trailer.
    let mut muxer_opts = ffmpeg::Dictionary::new();
    muxer_opts.set("movflags", "faststart");
    let unused = octx
        .write_header_with(muxer_opts)
        .map_err(|e| Error::muxing(format!("ffmpeg write_header: {e}")))?;
    drop(unused);

    // Remux every packet: reassign stream index, rescale timestamps to the
    // output stream's timebase, and write interleaved. Position/keyframe flags
    // are preserved by ffmpeg's packet infrastructure.
    for (stream, mut packet) in ictx.packets() {
        check()?;
        let Some(&osti) = stream_map.get(&stream.index()) else {
            // Unmapped stream (e.g. data/closed-caption) — skip.
            continue;
        };
        packet.set_stream(osti);
        if let Some(ostream) = octx.stream(osti) {
            packet.rescale_ts(stream.time_base(), ostream.time_base());
        }
        packet
            .write_interleaved(&mut octx)
            .map_err(|e| Error::muxing(format!("ffmpeg write_packet: {e}")))?;
    }

    check()?;
    octx.write_trailer()
        .map_err(|e| Error::muxing(format!("ffmpeg write_trailer: {e}")))?;

    // Input container duration (microseconds). May be AV_NOPTS_VALUE (i64::MIN)
    // for some sources; the caller clamps to 0.
    Ok(ictx.duration())
}

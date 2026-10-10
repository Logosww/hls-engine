# Fixed multi-track sessions

`MultiTrackSession` muxes a fixed selection using the continuous execution core.
Use `ContinuousMode::Vod` for finite ENDLIST snapshots, or `Open` for Live/EVENT.
The caller selects renditions, parses playlists and supplies subtitle cues. The
session does not select renditions, parse WebVTT files, change tracks or transcode.
See [player capabilities and limits](support.md) before enabling a player.

## Configure and run

The [native example](../examples/multitrack_demo.rs) writes selected video, two
AAC tracks and captions through Native classic finalization:

```sh
cargo run --example multitrack_demo -- video.m3u8 en.m3u8 ja.m3u8 output.mp4
```

Build `MultiTrackInputs::new(primary, EmbeddedAudio::Keep | Exclude)` explicitly.
`with_primary_audio(metadata)` describes retained main audio. Each new
`with_audio(input, metadata)` adds a selected audio input. Every media and subtitle
`InputId` must be unique. There are at most 32 media inputs and 32 subtitle tracks;
each media input keeps the existing at-most-one-video/one-audio demux boundary.
External input video is ignored. Excluding the only media in an audio-only main
input fails rather than inventing a video track.

`MultiTrackSession::new(inputs, keys, options)` validates the immutable selection.
Multiple inputs require a `ContinuousWait` host timer and bounded skew timeout,
including finite mode. Admit each snapshot with `handle.accept_snapshot(id, &s)`.
On `WouldBlock`, wait for capacity and retry the same snapshot. The convenience
`handle.control().accept_when_ready(id, &s).await` provides this media retry loop.
A slow writer blocks new reads and admission for **all** inputs. Sequence numbers
identify resources within a rendition; they never synchronize renditions.

Use explicit `ContinuousAnchor`s or program-date-time for independent epoch clocks.
The existing wrap, range, gap and configuration-change policies apply to every
media input. Default policies fail missing segments and configuration changes,
preserve gaps and require tail-duration evidence. An input's ENDLIST/`end_input`
does not truncate another input. `stop` drains accepted work; `cancel` aborts it.
The sink's caller owns close/shutdown and must await that before public completion.

`write_to` emits continuous fMP4 without an ever-growing `mfra`.
`into_bytes(capacity, format)` requires an explicit output-memory bound.
`write_to_file` with Native `StreamingMp4` uses disk staging and indexed classic
finalization. Native classic preserves AAC presentation gaps using a contiguous
decode timeline and composition offsets, keeping each encoded sample and its
duration unchanged. Accumulated audio composition offsets must fit signed 32-bit
track ticks; larger offsets fail finalization. Other tracks preserve gaps with
exact edit lists; video must resume at a sync sample and presentation runs must
not overlap. Set `with_decode_gaps(true)` when gaps are known. FFmpeg 9 does not
correctly play interior empty edits on those other tracks; use fMP4 for that case.
Its index grows with samples (including wvtt); this is distinct from
bounded fMP4 recording. `write_to_outputs` / `write_to_files` obtain separate leases
when the configured change policy requests a split. A single output cannot hide a
split. FFmpeg finalization is rejected for this entry before writing any output.

## Subtitle contract

For incremental sidecars and WebVTT resource decryption, see the
[acknowledged subtitle sink contract](subtitle-sidecars.md). Native fixed-file
adapters use `with_recoverable_subtitle_sink` for joint media/sidecar recovery.
Ordinary Writable sinks remain ineligible for recovery.

Add `SubtitleTrack::new(subtitle_input_id, timeline_input_id, metadata)` before
starting. Obtain its stable ID with `handle.subtitle_track_id`. Submit
`SubtitleCue::new(generation, epoch, start, end, payload)`, optionally using
`with_identifier` and `with_settings`. Times are on the **bound media input's source
clock**, not EXTINF estimates. `end_subtitles(track)` independently closes admission.

The supported profile is UTF-8 plain text plus `align`, `position`, `line`, `size`
and `vertical` settings. Omit `line` for automatic line placement; omit the
position alignment suffix for automatic alignment (for example `position:80%`).
Explicit `line:auto` / `position:80%,auto`, duplicate keys, signed/exponential
percentages and non-space/tab setting separators are rejected by the file grammar. Markup, STYLE, REGION and non-text profiles are rejected
with `UnsupportedSubtitleProfile`; malformed intervals get `InvalidSubtitle`.
Payload, identifier and accepted settings are retained as `payl`, `iden`, `sttg`.
Overlapping cues produce multiple `vttc` boxes within a single non-overlapping
sample; an empty interval is `vtte`. There is no tx3g conversion.

A fragment is sealed before bytes are handed to the writer. Fully late cues are
rejected in `SubtitleAcceptance` and retained report history; partially late cues
are clipped to the remaining interval. Subtitle gaps never wait for future cues.
Accepted cue tails drain at EOF/stop, bounded by a requested range. A cue without a
valid generation/epoch mapping fails with `MissingSubtitleMapping` at finalization.
Mapped cues spanning a fragment/epoch/output split retain their presentation
interval and identity; reports give each actual output interval. A new source clock
requires a cue with the corresponding generation/epoch, not a sequence-number guess.

Admission and generation share the operation's sample/byte limits with media.
Use `accept_cues_when_ready` to retry an atomic cue batch after consumer progress.
An intrinsically oversized batch fails `BudgetExceeded` without waiting.
Retained MAP bytes, queued payloads, mapping waits and overlap expansion count toward those limits;
excess admission returns `WouldBlock`, while expansion exceeding the configured
operation budget fails `BudgetExceeded`. Report history is bounded across the operation, with explicit
subtitle/track truncation flags. The current output always retains its fixed track
set; older track records share the history count/metadata budget. A very long single interval exceeding the MP4 u32 sample-duration field
fails `TimeOverflow`; the caller can divide it into shorter intervals.

## Metadata and reports

| Value | MP4 representation | Report |
| --- | --- | --- |
| Stable output track identity | `tkhd.track_ID`, fragment `tfhd` | `OutputTrackInfo::id` |
| Language | ISO-639 `mdhd` plus original language `elng` | metadata language |
| Name | UTF-8 `hdlr` name | metadata name |
| Default | enabled `tkhd` flag; alternate group 1 audio / 2 text | metadata default |
| Source identity | Not embedded as a URL | caller `InputId` |
| Clock mapping | timescale, DTS/PTS, offsets/edit lists | media mappings, including subtitle IDs |
| Cue identity and clipping | `iden`; actual sliced samples | bounded subtitle reports |

Common two-letter language tags map to ISO-639-2, valid three-letter tags are
retained; unsupported short tags use `und` in `mdhd` and retain their full value in
`elng`. At most one audio and one subtitle default may be declared. When no selected
audio is marked default, the first selected audio becomes default. Player-specific
selection semantics still require a playback gate.

`MultiTrackReport` contains legacy media accounting, separate extensible track
kind/codec metadata, cue receipts and configuration identity. That identity includes
selection, metadata, subtitle binding and mapping policy/anchors. It is **not** a
persistent checkpoint. Existing `TrackInfo`/`Codec`/`TrackType` literals, legacy
`with_audio()` replacement behavior and schema-v1 recovery remain unchanged.

## Packed AAC and combinations

Packed AAC is detected from decrypted resource bytes, independent of extension.
Each segment needs a starting ID3 v2.3/v2.4 PRIV timestamp owned by
`com.apple.streaming.transportStreamTimestamp`, with valid 33-bit content. MAP,
missing/duplicate/invalid anchors, corrupt ADTS, non-AAC-LC profiles and multiple
raw_data_blocks are rejected. Exact anchor-plus-sample-count timing survives 44.1
kHz and wrap; EXTINF is not the audio clock. Configuration changes within a segment
fail; changes between segments obey fail/split.

| Input | Clear | AES-128 resource CBC | SAMPLE-AES | SAMPLE-AES-CTR |
| --- | --- | --- | --- | --- |
| TS AVC/HEVC + AAC-LC | Yes | Yes | Existing TS supported profiles | No |
| fMP4 AVC/HEVC + AAC-LC | Yes | Yes | Existing cbcs profiles | Existing cenc profiles |
| Packed AAC-LC (new entry only) | Yes | Yes | AAC CBC frame profile | No |

`query_multitrack_capability` checks selected inputs, encryption, codec, operation
mode/range, sink, subtitle profile and playback target; input-specific rejections
carry `InputId`. Use `SubtitleProfile::WvttPlainText`. Styled WebVTT and FFmpeg finalization fail closed. ShakaAdapter denotes **single-text-track
extraction followed by the pinned MP4 VTT parser/display adapter**, not direct
mixed-file support or an audio extraction service. The application supplies this text-track adapter.

`playback_rejection()` distinguishes browser track selection, missing wvtt decoder,
subtitle settings fidelity, classic edit-list playback and decode-gap restrictions.
AvFoundation covers the tested macOS native file pipeline on continuous timelines.
VLC's vertical/position settings and IINA/FFmpeg's wvtt decoding fail the full profile.
Generic DirectBrowser is rejected after actual blob/MSE tests. FFmpeg/IINA classic
playback is rejected because the default edit-list path can lose/mis-time samples;
fMP4 A/V remains available explicitly. Container support never implies player support.

## WASM integration

[MultiRecorder](../examples/multitrack_wasm.rs) is a small clear-fixture bridge with
fixed audio IDs, cue JSON, decimal-string wide integers and a Promise writer.
Its 32 MiB resource map is an example host limit, not a production fetch cache.
Build with `--no-default-features --features serde --target wasm32-unknown-unknown
--example multitrack_wasm`, then use wasm-bindgen 0.2.100.

Migration
from a replaced audio input to several retained tracks requires the new entry,
explicit embedded-audio choice, stable input IDs, a host waiter and the separate
multi-track report. Existing consumers need no source/schema migration.

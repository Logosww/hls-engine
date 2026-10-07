# v0.10.0 acceptance

Baseline: published v0.9.0 (`fe97915`). Package name remains `hls-transmux`.
Implementation and acceptance are complete; registry publication has not been run.
The [machine record](release-0.10.0-evidence.json) requires all release checks to
pass and records no open gates. Unsupported player combinations below are explicit
preflight outcomes, not successful playback claims.

## Delivered surface

- `MultiTrackSession`, immutable selected inputs, explicit embedded-audio policy,
  stable output IDs, track metadata/configuration and separate reports.
- One continuous core for VOD and Live/EVENT: N-input scheduling, independent EOF,
  aggregate budgets, range/epoch mapping, gap policies and split output leases.
- Packed AAC ID3/ADTS parsing; clear, resource AES-128 and AAC SAMPLE-AES, including
  key rotation, exact 44.1 kHz clocks and 33-bit wrap.
- Bounded typed wvtt admission, overlap/empty intervals, late rejection/clipping,
  independent subtitle EOF, accepted tail drain and Native classic finalization.
- Additive capability queries with concrete `playback_rejection()` reasons.
  Legacy literals/enums, replacement-audio semantics and schema v1 remain intact.

The [guide](multitrack-sessions.md) defines supported layouts and migration. There
is no implicit conversion to tx3g, sample trimming, timeline compression or sidecar
fallback. STYLE/REGION/markup and unsupported syntax fail before cue admission.

The final player checks found a validation defect: `auto` is a default cue state,
not a valid explicit position-alignment or line token in the
[WebVTT file syntax](https://www.w3.org/TR/webvtt1/#webvtt-cue-settings).
The engine now rejects those tokens, duplicate settings, invalid numeric syntax
and line breaks. `position:80%` retains default automatic alignment correctly.
Both valid and invalid syntax have admission regression coverage.

## Reproducible evidence

| Check | Result |
| --- | --- |
| Five Rust feature combinations | Default, serde, all features, no default, no default + serde pass |
| Formatting and Clippy | Native all targets/features, runtime crate and WASM example pass |
| `scripts/verify_multitrack.py` | 194 outputs: 18 base, 160 range/epoch/encryption combinations, 16 configuration-split outputs |
| `scripts/verify_runtime.py --browser` | All 194 Native / Node WASM / Chrome output hashes and reports agree |
| `scripts/verify_sdk.py /path/to/hls-downloader` | Five multi-track cases, three cancellation cases and real SourceHost/Promise/writer integration; checkout unchanged |
| Legacy independent regressions | 148 timeline outputs and 48 dual-input outputs pass |
| Allocation probe | Bounded fMP4 history for 8/64/256 epochs; classic index growth reported separately |
| Player probes below | Both MP4 layouts, both languages, overlap, five setting groups, audio selection and direct MSE tested |
| `scripts/verify_package.py --allow-dirty` | Publish dry-run, archive contents, extracted legacy/new tests and native/WASM examples pass |

The 160-output matrix crosses TS/fMP4 video with TS/fMP4/Packed AAC audio,
resource/sample encryption and real key rotation, range/anchored epochs with gaps,
VOD/open-stop and classic/fMP4. Thirty-two vectors use Packed AAC as the primary.
Sixteen split outputs preserve two audio identities and spanning subtitles through
AVC→HEVC changes. Native disk-backed classic gap finalization agrees with the
in-memory result. Video resumes at sync samples; presentation runs cannot overlap.

## Playback decisions

The frozen D06 rule requires testing the target players and rejecting unsupported
combinations. Every listed target now has executed evidence; none is left pending.
The full accepted WebVTT settings profile must survive, not just its plain payload.

| Path | Observed outcome and enforced contract |
| --- | --- |
| Native classic/fMP4 container | Complete samples, exact rational timelines, metadata and cue intervals pass |
| Chrome 155 + Shaka 5.2.12 adapter | PASS: both languages/overlap, five setting groups, real DOM geometry, screenshots and extracted-DASH audio switching |
| macOS 27.0.1 AVFoundation / WebKit file playback | PASS for tested continuous-timeline files: five tracks, both audio/text selections, timed attributed decoding, all setting fields and native-rendered screenshots |
| Chrome direct blob / MSE | Full multi-track playback rejected: blob has no text tracks/audio selection API; MSE rejects wvtt SourceBuffer or fails actual append |
| WebKit direct MSE over HTTP | Actual SourceBuffer creation/append exercised; no embedded text tracks exposed; generic `DirectBrowser` rejected |
| VLC 3.0.24 | Audio/subtitle tracks discovered and selectable; actual screenshots show incorrect position and horizontal display of vertical cues; full wvtt profile rejected with `SubtitleSettings` |
| IINA 1.5.0 / mpv 0.41.0 | Official isolated distribution loads successfully, both audio tracks switch, neither layout exposes wvtt tracks; rejected with `WvttDecoder` |
| FFmpeg default classic playback | Initial empty edit trims 31/34 additional AAC samples; interior empty edits duplicate/mis-time packets; `Ffmpeg` classic target rejected with `ClassicEditTimeline` |
| FFmpeg default fMP4 A/V | Independent decoding passes; wvtt decoder capability is rejected separately |
| FFmpeg finalization backend | Rejected before output for this entry |

`AvFoundation` means the tested macOS native pipeline, with AVFoundation timed
subtitle decoding and companion WebKit native display evidence. Interior decode
gaps remain a container capability; native player declarations with gaps reject
`DecodeGaps`. This does not alter or compress the output timeline.

The FFmpeg classic mismatch also reproduces when FFmpeg remuxes its own fMP4 input.
Independent edit-list reconstruction and raw decoding prove sample integrity, and
are never substituted for a default playback pass. The capability regression tests
ensure that callers selecting FFmpeg/IINA classic playback receive rejection;
callers can explicitly select the verified fMP4 A/V path instead.

### Shaka adapter

Use [the pinned display adapter](../scripts/shaka_wvtt_adapter.mjs) after extracting
one wvtt track. Shaka's HTML renderer omits percentage `line:center` alignment; the
adapter adds the missing axis alignment without modifying cue payload or clocks.
It checks the exact debug version/internal hook and fails closed on another build.
The acceptance test checks the 20% cue midpoint and independent horizontal /
vertical-rl / vertical-lr centering in rendered geometry. This is a
shipped, reusable adapter, not an unshipped test-only modification of Shaka.

The [player verifier](../scripts/verify_multitrack_player.py) extracts original
sample bytes using independently probed offsets, builds single-text-track fragments
and renders them. Shaka cannot directly parse mixed audio/video/text mdat as text.
`ShakaAdapter` denotes that text extraction/display path; it does not turn mixed
files into directly playable MSE input or provide an audio extraction service.

```sh
# Optional browser-test dependency: npm install playwright (no browser download).
# Alternatively set HLS_PLAYWRIGHT to an existing playwright package directory.
python3 scripts/verify_multitrack_player.py --shaka target/multitrack/shaka-player-5.2.12.js
python3 scripts/verify_webkit.py
swiftc -module-cache-path target/swift-cache -parse-as-library scripts/probe_avfoundation.swift -o target/avfoundation-probe
target/avfoundation-probe target/multitrack/multitrack-fragmented.mp4 target/multitrack/multitrack-classic.mp4
```

Pinned external distributions (not vendored):

| Distribution | SHA-256 |
| --- | --- |
| [Shaka 5.2.12 compiled.debug](https://cdn.jsdelivr.net/npm/shaka-player@5.2.12/dist/shaka-player.compiled.debug.js) | `128ef0a049d1b2767615d10f6fd75fe31a5f84cfc9ab9604d5bc35813b9fb4ce` |
| [VLC 3.0.24 arm64](https://download.videolan.org/pub/vlc/3.0.24/macosx/vlc-3.0.24-arm64.dmg) | `64a89d93cdd30b0e97131743e246373db82d6826ea882d265d46d74b136da2b7` |
| [IINA 1.5.0](https://dl.iina.io/IINA.v1.5.0.dmg) | `8fad50479b10e09645053457cec46bd53d054ced2242cfe68f74586735a20dc7` |

The VLC and IINA probes accept `HLS_VLC_ROOT` (app Contents/MacOS directory) and
`HLS_IINA_LIBRARY` (libmpv path). Set `DYLD_LIBRARY_PATH` to the corresponding
bundled library directory and use a non-system Python so macOS preserves it.
They run isolated distributions without changing installed applications. The
WebKit probe opens/closes its own test window and serves fixtures on localhost;
file-origin MSE timeouts are excluded from acceptance.

Raw reports and screenshots are retained under `target/multitrack`; their hashes
are recorded in the machine evidence. No claim covers arbitrary player versions
or profiles beyond the tested support matrix. SDK integration is verified in an
isolated copy; product rollout and registry publication are deployment actions,
not unresolved implementation or acceptance work. v1.0 checkpoint/GCM/rename work
remains in its original version scope.

# Migrating to hls-engine 1.1

The crate now has one execution API: `EngineSession`. `legacy`, `HlsInput`,
`transmux_hls_*`, prepared/keyed/timeline sessions and schema-1 tasks are removed.
Source implementations, key providers and typed playlists remain reusable.

Applications read and select a media playlist, parse a `PlaylistSnapshot`, create
`EngineInputs` and submit snapshots through `EngineHandle`. Call `end_input` for
each finite input. For multiple audio tracks, choose `EmbeddedAudio::Keep` or
`Exclude` explicitly; `with_audio` adds a track rather than replacing one.
See [engine_demo](../examples/engine_demo.rs) and [Engine guide](engine.md).

Public `Engine*` types are directly defined by the Engine. The old lower-level
control handle is private; use `EngineHandle::progress` and `accept_when_ready`.
Capability checks use `query_capability` and `CapabilityQuery`; old session-specific
queries are removed. `Source` errors and media report types are publicly nameable.

The `ffmpeg-finalize` Cargo feature, backend selection and dependency are removed.
`FileOutputOptions::with_format` selects the output. Both `Mp4` and `StreamingMp4`
file output use the native finalizer; format enum names are unchanged. FFmpeg
remains an independent test oracle, not a production dependency.

Strict remains the default and reads/writes Engine schema 2/3. Repair uses schema 4
with a bound strategy/limit; these cannot be changed during restore. Schema 1 has
no automatic conversion and requires restarting the task. Existing frozen schema
2 and schema 3 archives are tested separately from new recovery round trips.

Regression coverage now runs through Engine for the retained media corpus,
sample encryption, continuous input, multiple tracks, subtitles and recovery.
Removed tests targeted retired entrypoints and their prepare/batch/schema-1
contracts; media parsing and crypto validation remain covered by core unit tests,
resource tests, shared native/WASM corpora and Engine integration tests.

# Capabilities and player limits

Use `query_capability` with the intended input, encryption, codec, output, range,
recovery, multi-track and subtitle dimensions. Admission, clock alignment,
resource identity and media validation are runtime requirements. A supported
container does not guarantee that a player can render every selected track.

## Core profile

| Input | Media | Encryption |
| --- | --- | --- |
| MPEG-TS | AVC/HEVC, AAC-LC | Clear, AES-128 resources, supported TS SAMPLE-AES |
| Fragmented MP4 | AVC/HEVC, AAC-LC | Clear, AES-128 resources, supported `cbcs`/`cenc` |
| Packed AAC | Validated ADTS and ID3 transport timestamps | Clear, AES-128, supported sample encryption |

The experimental resource GCM profile requires both the Cargo feature and runtime
opt-in. Its 32-byte key, 16-byte resource IV and 16-byte tag follow the fixed
HLS draft-22 profile. Authentication must succeed before plaintext submission.
See [sample encryption](sample-encryption.md) and [Engine GCM](engine.md#experimental-gcm)
for exact protection layouts and rejection rules.

Fixed multi-track sessions select AAC audio and application-supplied WebVTT cues.
Track membership is immutable. Independent input clocks need explicit anchors or
sufficient program-date-time evidence. Gap and configuration-change policies are
explicit, and video continuation must satisfy random-access requirements.

## Playback constraints

| Player/path | Contract |
| --- | --- |
| AVFoundation native files | Compatible files support track selection and `wvtt`; query the exact layout |
| VLC | Audio/video playback and audio selection; unsupported cue settings/style combinations are rejected |
| IINA/mpv | Audio/video and audio selection; in-container `wvtt` rendering is not supported by this profile |
| WebKit native file | Compatible file playback and subtitle rendering; direct multi-track MSE remains rejected |
| Shaka | `wvtt` needs application extraction to a text track; direct mixed-`mdat` subtitle playback is not supported |
| FFmpeg classic playback | Interior empty edits are not rendered correctly; select fragmented output for this case |

These constraints describe the supported integration paths, not a promise across
all player releases. Select a player-specific capability profile and obey its
rejection reason before presenting a combination to users.

## Persistence

Native recoverable fMP4/classic outputs require a filesystem supporting
same-directory hard links. The caller persists checkpoints and directory entries,
rebuilds source/key providers and replays the required bounded window. Generic
borrowed writers, collected bytes, WASM filesystem recovery and FFmpeg multi-track
recovery are not supported persistence targets.

DRM/CDM, license exchange, LL-HLS partial segments, transcoding and dynamic track
replacement are outside the supported profile.

//! v0.5 exhaustive enums stay closed; new playlist enums do not extend them.
use hls_engine::legacy::{InputRole, SessionPhase};

#[test]
fn v050_role_and_phase_matches_still_compile() {
    let role = InputRole::Primary;
    match role {
        InputRole::Primary | InputRole::Audio => {}
    }
    let phase = SessionPhase::Preparing;
    match phase {
        SessionPhase::Preparing
        | SessionPhase::Playlist
        | SessionPhase::Initialization
        | SessionPhase::Downloading
        | SessionPhase::Processing
        | SessionPhase::Writing
        | SessionPhase::Finalizing
        | SessionPhase::Completed => {}
    }
}

#[test]
fn legacy_error_enum_still_has_the_original_exhaustive_shape() {
    match hls_engine::legacy::Error::Cancelled {
        hls_engine::legacy::Error::Io(_)
        | hls_engine::legacy::Error::Http(_)
        | hls_engine::legacy::Error::InvalidInput(_)
        | hls_engine::legacy::Error::Unsupported(_)
        | hls_engine::legacy::Error::Bitstream(_)
        | hls_engine::legacy::Error::Muxing(_)
        | hls_engine::legacy::Error::Cancelled => {}
    }
}

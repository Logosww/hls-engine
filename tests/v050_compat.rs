//! v0.5 exhaustive enums stay closed; new playlist enums do not extend them.
use hls_transmux::{InputRole, SessionPhase};

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
    match hls_transmux::Error::Cancelled {
        hls_transmux::Error::Io(_)
        | hls_transmux::Error::Http(_)
        | hls_transmux::Error::InvalidInput(_)
        | hls_transmux::Error::Unsupported(_)
        | hls_transmux::Error::Bitstream(_)
        | hls_transmux::Error::Muxing(_)
        | hls_transmux::Error::Cancelled => {}
    }
}

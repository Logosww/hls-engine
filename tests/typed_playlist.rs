use hls_transmux::{SourceLocation, TextResource, parse_playlist_snapshot, playlist::*};

fn resource(content: &str) -> TextResource {
    TextResource {
        content: content.to_owned(),
        location: SourceLocation::Url(
            url::Url::parse("https://final.test/redirect/list.m3u8?manifest=secret").unwrap(),
        ),
    }
}
fn context() -> PlaylistContext {
    PlaylistContext::new(InputId::new("primary").unwrap(), 7).with_revision(11)
}
fn parse(content: &str) -> PlaylistSnapshot {
    parse_playlist_snapshot(&resource(content), context()).unwrap()
}
fn vod(body: &str) -> String {
    format!("#EXTM3U\n#EXT-X-TARGETDURATION:4\n{body}\n#EXT-X-ENDLIST\n")
}
fn failure(text: &str, kind: PlaylistErrorKind) {
    let error = parse_playlist_snapshot(&resource(text), context()).unwrap_err();
    assert_eq!(error.kind(), kind, "{text}: {error}");
}

#[test]
fn exact_metadata_uses_final_location_and_original_sequences() {
    let p = parse(&vod(
        "#EXT-X-VERSION:7\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-INDEPENDENT-SEGMENTS\n#EXT-X-MEDIA-SEQUENCE:9007199254740993\n#EXT-X-DISCONTINUITY-SEQUENCE:42\n#EXT-X-PROGRAM-DATE-TIME:2024-02-29T12:00:01.123456789+08:00\n#EXTINF:1.000000001,title\n../a.ts?sig=one\n#EXTINF:2.500000000,\nb.ts",
    ));
    assert_eq!(p.version(), Some(7));
    assert_eq!(p.playlist_type(), Some(&PlaylistType::Vod));
    assert!(p.end_list() && p.independent_segments());
    assert_eq!(p.target_duration(), Some(4));
    assert_eq!(p.media_sequence(), 9_007_199_254_740_993);
    assert_eq!(p.discontinuity_sequence(), 42);
    let a = &p.segments()[0];
    assert_eq!(a.slot().sequence(), 9_007_199_254_740_993);
    assert_eq!(a.slot().epoch(), 42);
    assert_eq!(a.slot().generation(), 7);
    assert_eq!(a.slot().input_id().as_str(), "primary");
    assert_eq!(a.duration().ticks(), 1_000_000_001);
    assert_eq!(a.duration().timescale(), 1_000_000_000);
    assert!(a.duration().media_time().is_ok());
    assert_eq!(
        a.program_date_time(),
        Some("2024-02-29T12:00:01.123456789+08:00")
    );
    assert_eq!(
        a.location().location(),
        &SourceLocation::Url(url::Url::parse("https://final.test/a.ts?sig=one").unwrap())
    );
    assert_eq!(p.segments()[1].duration().ticks(), 25);
    assert_eq!(p.segments()[1].duration().timescale(), 10);
    assert_eq!(p.segments()[1].program_date_time(), None);
    assert_eq!(p.validate_finite_vod(), Ok(()));
}

#[test]
fn metadata_acceptance_is_separate_from_finite_preflight() {
    assert_eq!(
        parse("#EXTM3U\n").validate_finite_vod(),
        Err(PlaylistRejection::OpenInput)
    );
    assert_eq!(
        parse(&vod("")).validate_finite_vod(),
        Err(PlaylistRejection::Empty)
    );
    for (tag, rejection) in [
        ("#EXT-X-PLAYLIST-TYPE:EVENT", PlaylistRejection::Event),
        ("#EXT-X-GAP", PlaylistRejection::Gap),
        ("#EXT-X-DISCONTINUITY", PlaylistRejection::Discontinuity),
        ("#EXT-X-I-FRAMES-ONLY", PlaylistRejection::IFrameOnly),
        (
            "#EXT-X-FUTURE:VALUE=secret",
            PlaylistRejection::UnvalidatedTag,
        ),
    ] {
        let p = parse(&vod(&format!("{tag}\n#EXTINF:1,\na.ts")));
        assert_eq!(p.validate_finite_vod(), Err(rejection));
    }
    let p = parse(&vod(
        "#EXT-X-DISCONTINUITY-SEQUENCE:8\n#EXTINF:1,\na\n#EXT-X-DISCONTINUITY\n#EXT-X-GAP\n#EXTINF:1,\nb",
    ));
    assert_eq!(p.segments()[1].slot().epoch(), 9);
    assert!(p.segments()[1].gap() && p.segments()[1].discontinuity());
    assert_eq!(
        parse("#EXTM3U\n#EXTINF:1,\na\n#EXT-X-ENDLIST").validate_finite_vod(),
        Err(PlaylistRejection::MissingTargetDuration)
    );
    assert_eq!(
        parse(&vod("#EXTINF:4.499999999,\na")).validate_finite_vod(),
        Ok(())
    );
    assert_eq!(
        parse(&vod("#EXTINF:4.5,\na")).validate_finite_vod(),
        Err(PlaylistRejection::DurationExceedsTarget)
    );
}

#[test]
fn key_candidates_rotate_and_maps_freeze_declaration_context() {
    let p = parse(&vod(
        "#EXT-X-KEY:METHOD=AES-128,URI=\"keys/k\",IV=0x1\n#EXT-X-MAP:URI=\"init\",BYTERANGE=\"16@0\"\n#EXTINF:1,\na\n#EXT-X-KEY:METHOD=AES-128,URI=\"keys/k\",IV=0x2\n#EXT-X-KEY:METHOD=AES-128,URI=\"provider:key\",KEYFORMAT=\"vendor\",KEYFORMATVERSIONS=\"2/1/2\",IV=0x2\n#EXTINF:1,\nb\n#EXT-X-KEY:METHOD=NONE\n#EXTINF:1,\nc",
    ));
    let [a, b, c] = p.segments() else { panic!() };
    let old = &a.keys().candidates()[0];
    let new = &b.keys().candidates()[0];
    assert_eq!(old.location(), new.location());
    assert_ne!(old.declaration(), new.declaration());
    assert_eq!(old.explicit_iv().unwrap()[15], 1);
    assert_eq!(new.explicit_iv().unwrap()[15], 2);
    assert_eq!(b.keys().candidates().len(), 2);
    assert_eq!(b.keys().candidates()[1].versions(), &[1, 2]);
    assert!(c.keys().is_clear());
    for s in p.segments() {
        assert_eq!(s.map().unwrap().keys().candidates(), a.keys().candidates());
    }
    assert_eq!(p.validate_finite_vod(), Ok(()));
    let repeated = parse(&vod(
        "#EXT-X-KEY:METHOD=AES-128,URI=\"k\"\n#EXTINF:1,\na\n#EXT-X-KEY:METHOD=AES-128,URI=\"k\"\n#EXTINF:1,\nb",
    ));
    assert_ne!(repeated.segments()[0].keys(), repeated.segments()[1].keys());
}

#[test]
fn unsupported_encryption_is_retained_and_rejected_before_execution() {
    for (method, rejection) in [
        ("SAMPLE-AES", PlaylistRejection::SampleEncryption),
        ("SAMPLE-AES-CTR", PlaylistRejection::SampleEncryption),
        ("AES-256-GCM", PlaylistRejection::UnknownEncryption),
        ("FUTURE", PlaylistRejection::UnknownEncryption),
    ] {
        let p = parse(&vod(&format!(
            "#EXT-X-KEY:METHOD={method},URI=\"k\"\n#EXTINF:1,\na"
        )));
        assert_eq!(
            p.segments()[0].keys().candidates()[0].method().as_str(),
            method
        );
        assert_eq!(p.validate_finite_vod(), Err(rejection));
    }
    let p = parse(&vod(
        "#EXT-X-KEY:METHOD=AES-128,URI=\"k\"\n#EXT-X-MAP:URI=\"init\"\n#EXT-X-KEY:METHOD=NONE\n#EXTINF:1,\na",
    ));
    assert_eq!(
        p.validate_finite_vod(),
        Err(PlaylistRejection::MissingMapIv)
    );
    let p = parse(&vod(
        "#EXT-X-KEY:METHOD=AES-128,URI=\"k\",VENDOR=\"retained\"\n#EXTINF:1,\na",
    ));
    assert_eq!(
        p.segments()[0].keys().candidates()[0].extensions()["VENDOR"],
        "retained"
    );
    assert_eq!(
        p.validate_finite_vod(),
        Err(PlaylistRejection::UnvalidatedTag)
    );
    let p = parse(&vod(
        "#EXT-X-KEY:METHOD=AES-128,URI=\"k\"\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"k\",KEYFORMAT=\"vendor\"\n#EXTINF:1,\na",
    ));
    assert_eq!(
        p.validate_finite_vod(),
        Err(PlaylistRejection::ConflictingKeyMethods)
    );
}

#[test]
fn implicit_ranges_require_preceding_media_range_on_same_resource() {
    let p = parse(&vod(
        "#EXT-X-MAP:URI=\"init\",BYTERANGE=\"10@4\"\n#EXT-X-BYTERANGE:16@9007199254740993\n#EXTINF:1,\nblob\n#EXT-X-BYTERANGE:32\n#EXTINF:1,\n./blob\n#EXT-X-MAP:URI=\"blob\",BYTERANGE=\"8\"\n#EXT-X-BYTERANGE:4\n#EXTINF:1,\nblob",
    ));
    assert_eq!(
        p.segments()[1].range().unwrap().offset(),
        9_007_199_254_741_009
    );
    let third = &p.segments()[2];
    assert_eq!(third.range().unwrap().offset(), 9_007_199_254_741_041);
    assert_eq!(
        third.map().unwrap().range().unwrap().offset(),
        third.range().unwrap().offset()
    );
    assert_eq!(third.range().unwrap().byte_range().length, 4);
    for body in [
        "#EXT-X-BYTERANGE:16\n#EXTINF:1,\nblob",
        "#EXT-X-MAP:URI=\"blob\",BYTERANGE=\"16\"\n#EXTINF:1,\nblob",
        "#EXT-X-MAP:URI=\"blob\",BYTERANGE=\"16@0\"\n#EXT-X-BYTERANGE:16\n#EXTINF:1,\nblob",
        "#EXT-X-BYTERANGE:16@0\n#EXTINF:1,\na\n#EXT-X-BYTERANGE:16\n#EXTINF:1,\nb",
        "#EXT-X-BYTERANGE:16@0\n#EXTINF:1,\na\n#EXTINF:1,\na\n#EXT-X-BYTERANGE:16\n#EXTINF:1,\na",
        "#EXT-X-BYTERANGE:0@0\n#EXTINF:1,\na",
    ] {
        failure(&vod(body), PlaylistErrorKind::InvalidRange);
    }
    failure(
        &vod("#EXT-X-BYTERANGE:1@18446744073709551615\n#EXTINF:1,\na"),
        PlaylistErrorKind::Overflow,
    );
}

#[test]
fn session_keys_are_validated_hints_not_media_state() {
    let master = resource(
        "#EXTM3U\n#EXT-X-SESSION-KEY:METHOD=AES-128,URI=\"k\"\n#EXT-X-STREAM-INF:BANDWIDTH=1000\nlist.m3u8",
    );
    let hints = parse_session_keys(&master, &context()).unwrap();
    let p = parse(&vod(
        "#EXT-X-KEY:METHOD=AES-128,URI=\"k\",IV=0x1\n#EXT-X-MAP:URI=\"init\"\n#EXT-X-KEY:METHOD=NONE\n#EXTINF:1,\na",
    ));
    assert!(p.segments()[0].keys().is_clear());
    assert_eq!(p.validate_session_keys(&hints), Ok(()));
    let mismatch = parse_session_keys(
        &resource(&master.content.replace("AES-128", "SAMPLE-AES")),
        &context(),
    )
    .unwrap();
    assert_eq!(
        p.validate_session_keys(&mismatch).unwrap_err().kind(),
        PlaylistErrorKind::SessionKeyConflict
    );
    for body in [
        "#EXT-X-SESSION-KEY:METHOD=NONE",
        "#EXT-X-SESSION-KEY:METHOD=AES-128,URI=\"k\"\n#EXT-X-SESSION-KEY:METHOD=AES-128,URI=\"k\"",
    ] {
        assert_eq!(
            parse_session_keys(&resource(&format!("#EXTM3U\n{body}")), &context())
                .unwrap_err()
                .kind(),
            PlaylistErrorKind::InvalidKey
        );
    }
    assert_eq!(
        parse_session_keys(&resource(&vod("#EXTINF:1,\na")), &context())
            .unwrap_err()
            .kind(),
        PlaylistErrorKind::MixedPlaylist
    );
}

#[test]
fn checked_numeric_boundaries_and_date_time_validation() {
    let p = parse(&vod(
        "#EXT-X-MEDIA-SEQUENCE:18446744073709551615\n#EXTINF:1,\na",
    ));
    assert_eq!(p.segments()[0].slot().sequence(), u64::MAX);
    failure(
        &vod("#EXT-X-MEDIA-SEQUENCE:18446744073709551615\n#EXTINF:1,\na\n#EXTINF:1,\nb"),
        PlaylistErrorKind::Overflow,
    );
    failure(
        &vod(
            "#EXT-X-DISCONTINUITY-SEQUENCE:18446744073709551615\n#EXT-X-DISCONTINUITY\n#EXTINF:1,\na",
        ),
        PlaylistErrorKind::Overflow,
    );
    for v in ["-1", "+1", "1e3", "1.1", ""] {
        failure(
            &vod(&format!("#EXT-X-MEDIA-SEQUENCE:{v}\n#EXTINF:1,\na")),
            PlaylistErrorKind::InvalidInteger,
        );
    }
    failure(
        &vod("#EXTINF:18446744073709551615.1,\na"),
        PlaylistErrorKind::Overflow,
    );
    for v in ["1.", ".1", "1.0000000001", "1.2.3"] {
        failure(
            &vod(&format!("#EXTINF:{v},\na")),
            PlaylistErrorKind::InvalidDuration,
        );
    }
    for v in [
        "2023-02-29T00:00:00Z",
        "2024-13-01T00:00:00Z",
        "2024-01-01T24:00:00Z",
        "2024-01-01T00:00:00+24:00",
        "2024-01-01T00:00:00.Z",
        "2024-01-01T00:00:00",
        "秘密-secret",
    ] {
        failure(
            &vod(&format!("#EXT-X-PROGRAM-DATE-TIME:{v}\n#EXTINF:1,\na")),
            PlaylistErrorKind::InvalidDateTime,
        );
    }
}

#[test]
fn malformed_attributes_and_tag_placement_fail_with_safe_errors() {
    for key in [
        "METHOD=AES-128,URI=k",
        "METHOD=AES-128,URI=\"k",
        "METHOD=AES-128,URI=\"k\",URI=\"other\"",
        "METHOD=AES-128,URI=\"k\",",
        "METHOD=\"AES-128\",URI=\"k\"",
    ] {
        failure(
            &vod(&format!("#EXT-X-KEY:{key}\n#EXTINF:1,\na")),
            PlaylistErrorKind::InvalidAttribute,
        );
    }
    for iv in ["1", "0x", "0xGG", "0x100000000000000000000000000000000"] {
        failure(
            &vod(&format!(
                "#EXT-X-KEY:METHOD=AES-128,URI=\"k\",IV={iv}\n#EXTINF:1,\na"
            )),
            PlaylistErrorKind::InvalidIv,
        );
    }
    failure(
        &vod("#EXT-X-KEY:METHOD=NONE,URI=\"k\"\n#EXTINF:1,\na"),
        PlaylistErrorKind::InvalidKey,
    );
    failure(
        &vod("#EXT-X-TARGETDURATION:4\n#EXTINF:1,\na"),
        PlaylistErrorKind::DuplicateTag,
    );
    failure(
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\na",
        PlaylistErrorKind::MasterPlaylist,
    );
    failure(
        &vod("#EXTINF:1,\na\n#EXT-X-MEDIA:TYPE=AUDIO"),
        PlaylistErrorKind::MixedPlaylist,
    );
    for body in [
        "#EXTINF:1,",
        "#EXT-X-GAP",
        "#EXTINF:1,\na\n#EXT-X-MEDIA-SEQUENCE:4",
        "a",
        "#EXT-X-ENDLIST:bad",
    ] {
        failure(&vod(body), PlaylistErrorKind::Syntax);
    }
    let e = parse_playlist_snapshot(
        &resource("#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:secret"),
        context(),
    )
    .unwrap_err();
    assert_eq!(e.line(), Some(2));
    assert!(!format!("{e:?} {e}").contains("secret"));
    assert!(InputId::new("").is_err());
    assert!(InputId::new("input\nsecret").is_err());
}

#[test]
fn resource_identity_separates_slots_rewrites_and_unreconciled_revisions() {
    let base = vod(
        "#EXT-X-KEY:METHOD=AES-128,URI=\"k\",IV=0x1\n#EXT-X-MAP:URI=\"init\"\n#EXTINF:1,\na?sig=one",
    );
    let p = parse(&base);
    let a = &p.segments()[0];
    assert_eq!(
        a.compare_resource(&parse(&base).segments()[0]),
        ResourceComparison::Duplicate
    );
    for changed in [
        base.replace("sig=one", "sig=two"),
        base.replace("0x1", "0x2"),
        base.replace("URI=\"init\"", "URI=\"init2\""),
        base.replace("#EXTINF:1", "#EXTINF:2"),
        base.replace("#EXTINF:1", "#EXT-X-GAP\n#EXTINF:1"),
        base.replace(
            "#EXTINF:1",
            "#EXT-X-PROGRAM-DATE-TIME:2024-01-01T00:00:00Z\n#EXTINF:1",
        ),
    ] {
        assert_eq!(
            a.compare_resource(&parse(&changed).segments()[0]),
            ResourceComparison::Rewritten
        );
    }
    for c in [
        PlaylistContext::new(InputId::new("other").unwrap(), 7),
        PlaylistContext::new(InputId::new("primary").unwrap(), 8),
    ] {
        let p = parse_playlist_snapshot(&resource(&base), c).unwrap();
        assert_eq!(
            a.compare_resource(&p.segments()[0]),
            ResourceComparison::DifferentSlot
        );
    }
    let next = parse_playlist_snapshot(&resource(&base), context().with_revision(12)).unwrap();
    assert_eq!(
        a.compare_resource(&next.segments()[0]),
        ResourceComparison::NeedsReconciliation
    );
}

#[test]
fn diagnostics_hide_credentials_opaque_keys_and_unknown_tag_payloads() {
    let text = vod(
        "#EXT-X-KEY:METHOD=AES-128,URI=\"https://USER_SECRET:PASS_SECRET@keys.test/key?QUERY_SECRET#FRAG_SECRET\",KEYFORMAT=\"FORMAT_SECRET\",VENDOR=\"ATTR_SECRET\"\n#EXT-X-FUTURE:TAG_SECRET\n#EXTINF:1,\na",
    );
    let p = parse(&text);
    let debug = format!("{p:?} {:?} {:?}", p.segments(), p.retained_tags());
    for secret in [
        "USER_SECRET",
        "PASS_SECRET",
        "QUERY_SECRET",
        "FRAG_SECRET",
        "FORMAT_SECRET",
        "ATTR_SECRET",
        "TAG_SECRET",
        "manifest=secret",
    ] {
        assert!(!debug.contains(secret), "{debug}");
    }
    let opaque = parse(&vod(
        "#EXT-X-KEY:METHOD=AES-128,URI=\"provider:KEY_BYTES_SECRET\"\n#EXTINF:1,\na",
    ));
    let loc = opaque.segments()[0].keys().candidates()[0].location();
    assert_eq!(loc.diagnostic(), "[REDACTED]");
    assert!(!format!("{opaque:?} {:?}", opaque.segments()).contains("KEY_BYTES_SECRET"));
}

#[test]
fn file_resources_resolve_relative_media_and_provider_keys() {
    let r = TextResource {
        content: vod("#EXT-X-KEY:METHOD=AES-128,URI=\"provider:key\"\n#EXTINF:1,\na.ts"),
        location: SourceLocation::File(std::path::PathBuf::from("fixtures/list.m3u8")),
    };
    let p = parse_playlist_snapshot(&r, context()).unwrap();
    assert_eq!(
        p.segments()[0].location().location(),
        &SourceLocation::File(std::path::PathBuf::from("fixtures/a.ts"))
    );
    assert_eq!(
        p.segments()[0].keys().candidates()[0]
            .location()
            .diagnostic(),
        "[REDACTED]"
    );
}

#[cfg(feature = "serde")]
#[test]
fn serde_archive_is_lossless_and_rejects_numeric_or_forged_projections() {
    use serde_json::{Value, json};
    let p = parse(&vod(
        "#EXT-X-MEDIA-SEQUENCE:9007199254740993\n#EXT-X-DISCONTINUITY-SEQUENCE:18446744073709551615\n#EXT-X-KEY:METHOD=AES-128,URI=\"k?secret=transport\",IV=0xffffffffffffffffffffffffffffffff\n#EXT-X-BYTERANGE:16@9007199254740993\n#EXTINF:1.000000001,\na",
    ));
    let value = serde_json::to_value(&p).unwrap();
    assert_eq!(value["media_sequence"], "9007199254740993");
    assert_eq!(value["segments"][0]["slot"]["epoch"], u64::MAX.to_string());
    assert_eq!(value["segments"][0]["range"]["offset"], "9007199254740993");
    assert_eq!(value["segments"][0]["duration"]["ticks"], "1000000001");
    let json = serde_json::to_string(&value).unwrap();
    assert!(json.contains("secret=transport")); // Transport serialization is deliberately not a diagnostic.
    assert_eq!(serde_json::from_str::<PlaylistSnapshot>(&json).unwrap(), p);
    for (pointer, replacement) in [
        ("/media_sequence", json!(9007199254740993u64)),
        ("/media_sequence", json!("18446744073709551616")),
        ("/media_sequence", json!("-1")),
        ("/media_sequence", json!("1e3")),
        ("/segments/0/range/offset", json!(9007199254740993u64)),
        ("/segments/0/duration/ticks", json!(1000000001u64)),
        ("/segments/0/duration/timescale", json!(0)),
        ("/segments/0/keys/candidates/0/iv/0", json!(0)),
        (
            "/segments/0/location/value",
            json!("https://attacker.test/a"),
        ),
        ("/schema_version", json!(2)),
    ] {
        let mut changed = value.clone();
        *changed.pointer_mut(pointer).unwrap() = replacement;
        assert!(
            serde_json::from_value::<PlaylistSnapshot>(changed).is_err(),
            "{pointer}"
        );
    }
    let mut nested = value.clone();
    nested["segments"][0]["location"]["unexpected"] = Value::Bool(true);
    assert!(serde_json::from_value::<PlaylistSnapshot>(nested).is_err());
    let mut changed = value.clone();
    changed["unexpected"] = Value::Bool(true);
    assert!(serde_json::from_value::<PlaylistSnapshot>(changed).is_err());
}

#[tokio::test]
async fn prepared_api_keeps_rejecting_key_including_none_before_media_reads() {
    use hls_transmux::{HlsInput, HlsInputs, MemorySource, PrepareOptions, prepare_hls};
    use std::sync::Arc;
    for key in ["METHOD=NONE", "METHOD=AES-128,URI=\"k\""] {
        let text = vod(&format!("#EXT-X-KEY:{key}\n#EXTINF:1,\na"));
        assert!(parse(&text).validate_finite_vod().is_ok());
        let source = MemorySource::new().text("https://old.test/list", text);
        let input = HlsInput::custom(
            Arc::new(source),
            SourceLocation::Url(url::Url::parse("https://old.test/list").unwrap()),
        );
        let error = prepare_hls(HlsInputs::new(input), PrepareOptions::default())
            .await
            .err()
            .unwrap();
        assert!(matches!(error.error(), hls_transmux::Error::Unsupported(_)));
        assert_eq!(error.phase(), hls_transmux::SessionPhase::Playlist);
    }
}

#[test]
fn absolute_native_file_paths_are_not_provider_schemes() {
    let media = std::env::temp_dir().join("hls-typed-media.ts");
    let resource = TextResource {
        content: vod(&format!("#EXTINF:1,\n{}", media.display())),
        location: SourceLocation::File(std::path::PathBuf::from("list.m3u8")),
    };
    let snapshot = parse_playlist_snapshot(&resource, context()).unwrap();
    assert_eq!(
        snapshot.segments()[0].location().location(),
        &SourceLocation::File(media)
    );
}

#[tokio::test]
async fn prepared_vod_without_endlist_remains_supported() {
    use hls_transmux::{HlsInput, HlsInputs, MemorySource, PrepareOptions, prepare_hls};
    use std::sync::Arc;
    let text = "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\na.ts\n";
    assert_eq!(
        parse(text).validate_finite_vod(),
        Err(PlaylistRejection::OpenInput)
    );
    let source = MemorySource::new()
        .text("https://old.test/list", text)
        .segment(
            "https://old.test/a.ts",
            include_bytes!("fixtures/media/ts_avc_regular/seg0.ts").to_vec(),
        );
    let input = HlsInput::custom(
        Arc::new(source),
        SourceLocation::Url(url::Url::parse("https://old.test/list").unwrap()),
    );
    let session = prepare_hls(HlsInputs::new(input), PrepareOptions::default())
        .await
        .unwrap();
    assert!(!session.into_mp4_bytes().await.unwrap().0.is_empty());
}

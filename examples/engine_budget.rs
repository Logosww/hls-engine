//! Requested-allocation and checkpoint growth across bounded rolling recordings.
#[path = "../tests/support/allocation.rs"]
mod allocation;
#[allow(dead_code)]
#[path = "../tests/support/sample_crypto.rs"]
mod sample_corpus;
use hls_engine::{crypto::key::*, playlist::*, *};
use std::collections::BTreeMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
fn id(name: &str) -> InputId {
    InputId::new(name).unwrap()
}
fn snapshot(input: &InputId, start: u64, count: u64, encrypted: bool) -> PlaylistSnapshot {
    let mut text = format!(
        "#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXT-X-MEDIA-SEQUENCE:{start}\n#EXT-X-DISCONTINUITY-SEQUENCE:{start}\n#EXT-X-MAP:URI=\"init\"\n"
    );
    if encrypted {
        text = text.replace(
            "#EXT-X-MAP:",
            "#EXT-X-KEY:METHOD=AES-256-GCM,URI=\"key\"\n#EXT-X-MAP:",
        );
    }
    for n in 0..count {
        if n > 0 {
            text.push_str("#EXT-X-DISCONTINUITY\n");
        }
        if encrypted {
            text.push_str(if (start + n).is_multiple_of(2) {
                "#EXT-X-KEY:METHOD=AES-256-GCM,URI=\"key\"\n"
            } else {
                "#EXT-X-KEY:METHOD=AES-256-GCM,URI=\"rotated\"\n"
            });
        }
        let nanos = (start + n) * 94 * 1024 * 1_000_000_000 / 48000;
        let seconds = nanos / 1_000_000_000;
        text.push_str(&format!("#EXT-X-PROGRAM-DATE-TIME:2026-10-06T{:02}:{:02}:{:02}.{:09}Z\n#EXTINF:2.005333333,\nmedia\n", seconds / 3600, seconds / 60 % 60, seconds % 60, nanos % 1_000_000_000));
        if encrypted {
            let length = text.len() - "media\n".len();
            text.truncate(length);
            text.push_str(if (start + n).is_multiple_of(2) {
                "even\n"
            } else {
                "odd\n"
            });
        }
    }
    parse_playlist_snapshot(
        &TextResource {
            location: SourceLocation::Url(url::Url::parse("https://budget.test/index").unwrap()),
            content: text,
        },
        PlaylistContext::new(input.clone(), 0).with_revision(start + count),
    )
    .unwrap()
}
fn create(
    count: u64,
    names: &[&str],
    encrypted: bool,
    cp: Option<EngineCheckpoint>,
    committed: Arc<Mutex<BTreeMap<String, u64>>>,
) -> EngineSession {
    let source = MemorySource::new()
        .segment(
            "https://budget.test/init",
            include_bytes!("../tests/fixtures/sample_crypto/fmp4_aac_clear/init.mp4"),
        )
        .segment(
            "https://budget.test/media",
            include_bytes!("../tests/fixtures/sample_crypto/fmp4_aac_clear/seg1.m4s"),
        );
    let source = if encrypted {
        MemorySource::new()
            .segment(
                "https://budget.test/init",
                include_bytes!("../tests/fixtures/gcm/resource-6.gcm"),
            )
            .segment(
                "https://budget.test/even",
                include_bytes!("../tests/fixtures/gcm/resource-7.gcm"),
            )
            .segment(
                "https://budget.test/odd",
                include_bytes!("../tests/fixtures/gcm/resource-8.gcm"),
            )
    } else {
        source
    };
    let holder = Arc::new(Mutex::new(None::<EngineHandle>));
    let callback = holder.clone();
    let progress = committed.clone();
    let options = EngineOptions::default()
        .with_mode(EngineMode::Open)
        .with_experimental_gcm(encrypted)
        .with_waiter(Arc::new(Wait), std::time::Duration::from_secs(1))
        .with_limits(
            EngineLimits::default()
                .with_queue(8, 64 * 1024)
                .with_history(4),
        )
        .with_on_event(Arc::new(move |event| {
            if let EngineEvent::Committed { input, .. } = event {
                let n = input.committed();
                progress
                    .lock()
                    .unwrap()
                    .insert(input.input_id().as_str().to_owned(), n);
                let guard = callback.lock().unwrap();
                let h = guard.as_ref().unwrap();
                if n == count {
                    h.end_input(input.input_id()).unwrap();
                } else {
                    h.accept_snapshot(
                        input.input_id(),
                        &snapshot(input.input_id(), n - 1, 2, encrypted),
                    )
                    .unwrap();
                }
            }
        }));
    let source = Arc::new(source);
    let mut inputs = EngineInputs::new(
        EngineInput::new(id(names[0]), source.clone()),
        EmbeddedAudio::Keep,
    );
    for name in &names[1..] {
        inputs = inputs.with_audio(
            EngineInput::new(id(name), source.clone()),
            TrackMetadata::new("en", *name),
        );
    }
    let keys: KeySession = sample_corpus::keys(Arc::new(Provider));
    let terminal = cp.as_ref().is_some_and(|c| c.is_finalizing());
    let session = match cp {
        Some(cp) => EngineSession::restore(inputs, keys, options, cp),
        None => EngineSession::new(inputs, keys, options),
    }
    .unwrap();
    let h = session.handle();
    *holder.lock().unwrap() = Some(h.clone());
    if !terminal {
        for name in names {
            let input = id(name);
            let n = committed.lock().unwrap().get(*name).copied().unwrap_or(0);
            let start = n.saturating_sub(1);
            h.accept_snapshot(
                &input,
                &snapshot(
                    &input,
                    start,
                    (count - start).min(if n == 0 { 1 } else { 2 }),
                    encrypted,
                ),
            )
            .unwrap();
            if n == count {
                h.end_input(&input).unwrap();
            }
        }
    }
    session
}
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let counts = std::env::var("HLS_ENGINE_BUDGET_COUNTS").unwrap_or_else(|_| "64,512,4096".into());
    let encrypted = std::env::var_os("HLS_ENGINE_BUDGET_GCM").is_some();
    assert!(
        !encrypted || cfg!(feature = "experimental-gcm"),
        "GCM benchmark requires experimental-gcm"
    );
    let mut rows = vec![];
    for count in counts.split(',').map(|n| n.parse::<u64>().unwrap()) {
        for names in [&["primary"][..], &["primary", "audio-en", "audio-ja"][..]] {
            for format in [OutputFormat::FragmentedMp4, OutputFormat::StreamingMp4] {
                let dir = std::env::temp_dir().join(format!(
                    "hls-engine-budget-{}-{count}-{format:?}",
                    std::process::id()
                ));
                std::fs::create_dir(&dir).unwrap();
                let path = dir.join("output.mp4");
                let progress = Arc::new(Mutex::new(BTreeMap::<String, u64>::new()));
                let checkpoint = Arc::new(Mutex::new(None));
                let saved = checkpoint.clone();
                let peak_checkpoint = Arc::new(AtomicUsize::new(0));
                let peak = peak_checkpoint.clone();
                let position = progress.clone();
                let (baseline, total) = allocation::reset();
                let started = std::time::Instant::now();
                create(count, names, encrypted, None, progress.clone())
                    .write_recoverable_to_file(
                        &path,
                        RecoveryOptions::new(Arc::new(move |cp| {
                            peak.fetch_max(cp.to_bytes().len(), Ordering::Relaxed);
                            *saved.lock().unwrap() = Some(cp);
                            if position
                                .lock()
                                .unwrap()
                                .values()
                                .copied()
                                .min()
                                .unwrap_or(0)
                                >= count / 2
                            {
                                return Err(EngineError::output(
                                    std::io::Error::other("recording interruption").into(),
                                ));
                            }
                            Ok(())
                        }))
                        .with_output_format(format),
                    )
                    .await
                    .unwrap_err();
                let (_, recording_peak, recording_total) = allocation::snapshot();
                let cp = checkpoint.lock().unwrap().take().unwrap();
                let (replay_base, _) = allocation::reset();
                let replay_started = std::time::Instant::now();
                let peak = peak_checkpoint.clone();
                let replay_first = Arc::new(Mutex::new(None));
                let first = replay_first.clone();
                let report = create(count, names, encrypted, Some(cp), progress.clone())
                    .write_recoverable_to_file(
                        &path,
                        RecoveryOptions::new(Arc::new(move |cp| {
                            peak.fetch_max(cp.to_bytes().len(), Ordering::Relaxed);
                            first.lock().unwrap().get_or_insert_with(|| {
                                replay_started.elapsed().as_secs_f64() * 1000.0
                            });
                            Ok(())
                        }))
                        .with_output_format(format),
                    )
                    .await
                    .unwrap();
                let (_, replay_peak, _) = allocation::snapshot();
                assert!(
                    report
                        .media()
                        .inputs()
                        .iter()
                        .all(|input| input.committed() == count)
                );
                rows.push(serde_json::json!({"segments":count,"encrypted":encrypted,"inputs":names.len(),"format":format!("{format:?}"),
                "mediaSeconds":count as f64 * 94.0 * 1024.0 / 48000.0,
                "recordingPeakIncrementBytes":recording_peak.saturating_sub(baseline),
                "recordingAllocatedBytes":recording_total.wrapping_sub(total),
                "replayPeakIncrementBytes":replay_peak.saturating_sub(replay_base),
                "maxCheckpointBytes":peak_checkpoint.load(Ordering::Relaxed),
                "firstResumedCheckpointMs":*replay_first.lock().unwrap(),
                "classicIndexSamples":report.media().outputs()[0].classic_index_samples(),
                "mediaPeaks":report.media().peaks(),"elapsedSeconds":started.elapsed().as_secs_f64(),
                "outputBytes":report.media().bytes_written()}));
                std::fs::remove_dir_all(dir).unwrap();
            }
        }
    }
    println!(
        "{}",
        serde_json::json!({"measurement":"requested allocations, not RSS; half-way replay in same process", "rows":rows})
    );
}

struct Wait;
impl EngineWait for Wait {
    fn wait(
        &self,
        _: std::time::Duration,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::pending())
    }
}

struct Provider;
impl KeyProvider for Provider {
    fn resolve(&self, request: KeyRequest) -> KeyFuture<KeyResolution> {
        #[cfg(feature = "experimental-gcm")]
        if request.reference().method() == &EncryptionMethod::Aes256Gcm {
            let rotated = matches!(request.reference().location().location(), SourceLocation::Url(url) if url.path().ends_with("rotated"));
            let start = if rotated { 32 } else { 0 };
            return Box::pin(async move {
                KeyResolution::Available(
                    AvailableKey::aes256_gcm(
                        SecretKey::aes256((start..start + 32).collect()).unwrap(),
                    )
                    .with_version("budget-fixture-v1"),
                )
            });
        }
        sample_corpus::Provider.resolve(request)
    }
}

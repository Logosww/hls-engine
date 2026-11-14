//! Manual I/O benchmark, not a codec/player test. Dense synthetic samples avoid
//! sparse-file/reflink shortcuts. Each method gets a newly generated source.
use super::*;
use std::fs::OpenOptions;
use std::time::Instant;

#[test]
#[ignore = "manual dense multi-GiB benchmark; writes substantial data"]
fn native_finalization_benchmark() {
    let sizes: Vec<u64> = std::env::var("HLS_FINALIZE_BENCH_MIB")
        .unwrap_or_else(|_| "256,2048,8192".into())
        .split(',')
        .map(|s| s.parse().unwrap())
        .collect();
    let repetitions: usize = std::env::var("HLS_FINALIZE_BENCH_REPEATS")
        .unwrap_or_else(|_| "3".into())
        .parse()
        .unwrap();
    let folder = std::env::temp_dir().join(format!("hls-dense-finalize-{}", std::process::id()));
    std::fs::create_dir(&folder).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(folder.clone());
    let mut seed = 0x123456789abcdef0u64;
    let payload: Vec<u8> = (0..16384)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as u8
        })
        .collect();
    for mib in sizes {
        assert!((64..=32768).contains(&mib));
        for repetition in 0..repetitions {
            let mut modes = vec![("copy", 0u64), ("in_place_8k", 8192)];
            if mib >= 8192 {
                modes.push(("in_place_1m", 1024 * 1024));
            }
            // Rotate order to distribute cache/load/thermal ordering effects.
            let n = modes.len();
            modes.rotate_left(repetition % n);
            for (mode, reserve) in modes {
                eprintln!(
                    "BENCH_START size_mib={mib} round={} mode={mode}",
                    repetition + 1
                );
                let source = folder.join("source.mp4");
                let target = folder.join("output.mp4");
                let prepared = Instant::now();
                let mut file = OpenOptions::new()
                    .create_new(true)
                    .read(true)
                    .write(true)
                    .open(&source)
                    .unwrap();
                if reserve > 0 {
                    file.write_all(&(reserve as u32).to_be_bytes()).unwrap();
                    file.write_all(b"free").unwrap();
                    file.write_all(&vec![0; reserve as usize - 8]).unwrap();
                }
                let mut mux = FragmentedMp4Muxer::new(vec![FragmentedTrack::audio(
                    1,
                    48000,
                    2,
                    vec![0x11, 0x90],
                )]);
                file.write_all(&mux.write_header().unwrap()).unwrap();
                let count = mib * 1024 * 1024 / payload.len() as u64;
                for first in (0..count).step_by(256) {
                    let samples = (first..(first + 256).min(count))
                        .map(|i| Mp4Sample {
                            data: payload.clone(),
                            source: None,
                            dts: i * 1024,
                            pts: i128::from(i * 1024),
                            duration: 1024,
                            is_key: true,
                            offset: 0,
                        })
                        .collect();
                    file.write_all(&mux.write_fragment(&[samples]).unwrap())
                        .unwrap();
                }
                file.sync_all().unwrap();
                let prepare_seconds = prepared.elapsed().as_secs_f64();
                let input_bytes = file.metadata().unwrap().len();
                #[cfg(unix)]
                let allocated_input_bytes = {
                    use std::os::unix::fs::MetadataExt;
                    file.metadata().unwrap().blocks() * 512
                };
                #[cfg(not(unix))]
                let allocated_input_bytes = 0u64;
                let started = Instant::now();
                let scan = crate::isobmff::scan_file(
                    &mut ReservedReader::new(&mut file, reserve),
                    input_bytes - reserve,
                    true,
                    false,
                    &|| Ok(()),
                )
                .unwrap();
                let sample_probes: Vec<(usize, u64)> = [0, count as usize / 2, count as usize - 1]
                    .into_iter()
                    .map(|i| (i, scan.samples[0][i].source.unwrap().0))
                    .collect();
                let tracks = crate::isobmff::file_tracks(&scan.init).unwrap();
                let scan_seconds = started.elapsed().as_secs_f64();
                let mux = Mp4Muxer::from_fragments(tracks, scan.samples);
                let mut output = if mode == "copy" {
                    Some(
                        OpenOptions::new()
                            .create_new(true)
                            .read(true)
                            .write(true)
                            .open(&target)
                            .unwrap(),
                    )
                } else {
                    None
                };
                let (output_bytes, _) = if let Some(output) = output.as_mut() {
                    mux.write_file(&mut file, output, &|| Ok(())).unwrap()
                } else {
                    mux.finalize_in_place(&mut file, &scan.init, input_bytes, reserve, &|| Ok(()))
                        .unwrap()
                };
                let result = output.as_mut().unwrap_or(&mut file);
                result.flush().unwrap();
                let finalize_seconds = started.elapsed().as_secs_f64();
                let sync_started = Instant::now();
                result.sync_all().unwrap();
                let sync_seconds = sync_started.elapsed().as_secs_f64();
                // Outside timing: validate front-loaded moov and three sample
                // payloads, including the last (>4 GiB) sample in large cases.
                let mut position = 0u64;
                let mut types = Vec::new();
                let mut payload_start = 0;
                while position < output_bytes {
                    result.seek(SeekFrom::Start(position)).unwrap();
                    let mut header = [0; 8];
                    result.read_exact(&mut header).unwrap();
                    let mut size = u64::from(u32::from_be_bytes(header[..4].try_into().unwrap()));
                    let mut width = 8;
                    if size == 1 {
                        result.read_exact(&mut header[..8]).unwrap();
                        size = u64::from_be_bytes(header);
                        width = 16;
                        result.seek(SeekFrom::Start(position + 4)).unwrap();
                        result.read_exact(&mut header[4..8]).unwrap();
                    }
                    let kind = header[4..8].to_vec();
                    if kind == b"mdat" {
                        payload_start = position + width;
                    }
                    types.push(kind);
                    assert!(size >= width);
                    position += size;
                }
                assert_eq!(position, output_bytes);
                assert_eq!(types[0], b"ftyp");
                assert_eq!(types[1], b"moov");
                let front = if mode == "copy" {
                    0
                } else {
                    output_bytes - (input_bytes - reserve)
                };
                if mode != "copy" {
                    assert!(front > reserve, "benchmark must exercise relocation");
                }
                let mut probe = vec![0; payload.len()];
                for (i, offset) in sample_probes {
                    let offset = if mode == "copy" {
                        payload_start + i as u64 * payload.len() as u64
                    } else {
                        offset + front
                    };
                    result.seek(SeekFrom::Start(offset)).unwrap();
                    result.read_exact(&mut probe).unwrap();
                    assert_eq!(probe, payload);
                }
                println!(
                    "BENCH {}",
                    serde_json::json!({"mode":mode,"payload_mib":mib,"repetition":repetition+1,"samples":count,"reserve_bytes":reserve,"input_bytes":input_bytes,"allocated_input_bytes":allocated_input_bytes,"output_bytes":output_bytes,"prepare_seconds":prepare_seconds,"scan_seconds":scan_seconds,"finalize_seconds":finalize_seconds,"sync_seconds":sync_seconds,"finalize_sync_seconds":finalize_seconds+sync_seconds,"pipeline_seconds":prepare_seconds+finalize_seconds,"pipeline_sync_seconds":prepare_seconds+finalize_seconds+sync_seconds,"faststart_and_sample_probes":true})
                );
                drop(output);
                drop(file);
                std::fs::remove_file(source).unwrap();
                if mode == "copy" {
                    std::fs::remove_file(target).unwrap();
                }
            }
        }
    }
}

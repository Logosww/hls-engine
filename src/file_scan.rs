//! Checkpoint-bounded scanner for the crate's recoverable fMP4 files.
use super::*;
use crate::mp4::{Mp4Sample, TfraEntry};
use std::io::{Read, Seek, SeekFrom};

pub(crate) struct FileIndex {
    pub init: Vec<u8>,
    pub samples: Vec<Vec<Mp4Sample>>,
    pub entries: Vec<Vec<TfraEntry>>,
    pub fragments: usize,
    pub sample_counts: Vec<usize>,
    pub decode_ends: Vec<u64>,
    pub presentation_ends: Vec<i128>,
    pub last_durations: Vec<u32>,
}

struct Header {
    kind: [u8; 4],
    start: u64,
    payload: u64,
    end: u64,
}

fn header<R: Read + Seek>(reader: &mut R, start: u64, limit: u64) -> Result<Header> {
    reader.seek(SeekFrom::Start(start))?;
    if limit.saturating_sub(start) < 8 {
        return Err(Error::invalid("truncated box header"));
    }
    let mut bytes = [0; 8];
    reader.read_exact(&mut bytes)?;
    let small = u32::from_be_bytes(bytes[..4].try_into().unwrap());
    let (size, width) = if small == 1 {
        if limit - start < 16 {
            return Err(Error::invalid("truncated extended box header"));
        }
        reader.read_exact(&mut bytes[..8])?;
        (u64::from_be_bytes(bytes), 16)
    } else if small == 0 {
        return Err(Error::invalid(
            "size-zero boxes are not valid checkpoint boxes",
        ));
    } else {
        (u64::from(small), 8)
    };
    // The extended-size read overwrites bytes; recover the type separately.
    reader.seek(SeekFrom::Start(start + 4))?;
    let mut kind = [0; 4];
    reader.read_exact(&mut kind)?;
    let end = start
        .checked_add(size)
        .filter(|&end| size >= width && end <= limit)
        .ok_or_else(|| Error::invalid("box extends past checkpoint"))?;
    Ok(Header {
        kind,
        start,
        payload: start + width,
        end,
    })
}

fn metadata<R: Read + Seek>(
    reader: &mut R,
    h: &Header,
    check: &dyn Fn() -> Result<()>,
) -> Result<Vec<u8>> {
    let size =
        usize::try_from(h.end - h.start).map_err(|_| Error::invalid("metadata box too large"))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(size)
        .map_err(|_| Error::invalid("metadata allocation failed"))?;
    reader.seek(SeekFrom::Start(h.start))?;
    let mut buffer = [0; 8192];
    while bytes.len() < size {
        check()?;
        let n = buffer.len().min(size - bytes.len());
        reader.read_exact(&mut buffer[..n])?;
        bytes.extend_from_slice(&buffer[..n]);
    }
    Ok(bytes)
}

/// Never reads media payload. Sample metadata and random-access index retention
/// are independent; validation always runs even when both are disabled.
pub(crate) fn scan<R: Read + Seek>(
    reader: &mut R,
    limit: u64,
    keep_samples: bool,
    keep_index: bool,
    check: &dyn Fn() -> Result<()>,
) -> Result<FileIndex> {
    check()?;
    if reader.seek(SeekFrom::End(0))? < limit {
        return Err(Error::invalid("file shorter than checkpoint bytes_written"));
    }
    let mut position = 0;
    let mut init = Vec::new();
    for expected in [*b"ftyp", *b"moov"] {
        let h = header(reader, position, limit)?;
        if h.kind != expected {
            return Err(Error::invalid("invalid fMP4 initialization layout"));
        }
        init.extend(metadata(reader, &h, check)?);
        position = h.end;
    }
    let tracks = parse_init_segment(&init)?;
    if tracks.iter().any(|t| t.timescale == 0) {
        return Err(Error::invalid("zero track timescale"));
    }
    let mut result = FileIndex {
        init,
        samples: (0..tracks.len()).map(|_| Vec::new()).collect(),
        entries: (0..tracks.len()).map(|_| Vec::new()).collect(),
        fragments: 0,
        sample_counts: vec![0; tracks.len()],
        decode_ends: vec![0; tracks.len()],
        presentation_ends: vec![0; tracks.len()],
        last_durations: vec![0; tracks.len()],
    };
    while position < limit {
        check()?;
        let styp = header(reader, position, limit)?;
        if styp.kind != *b"styp" {
            return Err(Error::invalid(
                "checkpoint is not at a complete fragment boundary",
            ));
        }
        let moof = header(reader, styp.end, limit)?;
        if moof.kind != *b"moof" {
            return Err(Error::invalid("missing moof"));
        }
        let mdat = header(reader, moof.end, limit)?;
        if mdat.kind != *b"mdat" {
            return Err(Error::invalid("missing mdat"));
        }
        let bytes = metadata(reader, &moof, check)?;
        let payload = &bytes[(moof.payload - moof.start) as usize..];
        let mfhd = find_box(payload, b"mfhd")?
            .ok_or_else(|| Error::invalid("missing fragment sequence"))?;
        result.fragments = result
            .fragments
            .checked_add(1)
            .ok_or_else(|| Error::invalid("fragment count overflow"))?;
        if mfhd.len() != 8
            || u64::from(u32::from_be_bytes(mfhd[4..8].try_into().unwrap()))
                != result.fragments as u64
        {
            return Err(Error::invalid("checkpoint fragment sequence mismatch"));
        }
        let mut traf_number = 0u32;
        let mut seen = Vec::new();
        for_each_box(payload, |kind, traf| {
            check()?;
            if kind != b"traf" {
                return Ok(());
            }
            traf_number += 1;
            let tfhd = parse_tfhd(
                find_box(traf, b"tfhd")?.ok_or_else(|| Error::invalid("missing tfhd"))?,
            )?;
            let ti = tracks
                .iter()
                .position(|t| t.track_id == tfhd.track_id)
                .ok_or_else(|| Error::invalid("unknown track_id"))?;
            if seen.contains(&ti) {
                return Err(Error::unsupported("multiple traf boxes for one track"));
            }
            seen.push(ti);
            let track = &tracks[ti];
            if tfhd.base_data_offset.is_none() && !tfhd.default_base_is_moof {
                return Err(Error::unsupported("implicit cross-traf base offsets"));
            }
            let mut dts = parse_tfdt(
                find_box(traf, b"tfdt")?.ok_or_else(|| Error::invalid("missing tfdt"))?,
            )?;
            dts = u64::try_from(i128::from(dts) + track.timeline_offset)
                .map_err(|_| Error::invalid("edited decode timestamp exceeds u64"))?;
            if result.sample_counts[ti] > 0 && dts < result.decode_ends[ti] {
                return Err(Error::invalid(
                    "fragment decode timeline overlaps committed samples",
                ));
            }
            let base = tfhd.base_data_offset.unwrap_or(moof.start);
            let mut offset = base;
            let mut run_number = 0u32;
            let mut indexed_sync = false;
            for_each_box(traf, |kind, data| {
                if kind != b"trun" {
                    return Ok(());
                }
                run_number += 1;
                let run = parse_trun(data, &tfhd, track)?;
                if let Some(delta) = run.data_offset {
                    offset = base
                        .checked_add_signed(i64::from(delta))
                        .ok_or_else(|| Error::invalid("sample offset overflow"))?;
                }
                for (si, sample) in run.samples.iter().enumerate() {
                    check()?;
                    let end = offset
                        .checked_add(u64::from(sample.size))
                        .filter(|&end| offset >= mdat.payload && end <= mdat.end)
                        .ok_or_else(|| Error::invalid("sample data extends past mdat"))?;
                    let pts = i128::from(dts) + i128::from(sample.composition_offset.unwrap_or(0));
                    let is_key =
                        matches!(track.kind, StreamKind::Aac) || sample.flags & 0x0001_0000 == 0;
                    if keep_index && is_key && !indexed_sync {
                        result.entries[ti].push(TfraEntry {
                            time: u64::try_from(i128::from(dts) - track.timeline_offset).map_err(
                                |_| Error::invalid("media decode timestamp exceeds u64"),
                            )?,
                            moof_offset: moof.start,
                            traf_number,
                            trun_number: run_number,
                            sample_number: u32::try_from(si + 1)
                                .map_err(|_| Error::invalid("sample count overflow"))?,
                        });
                        indexed_sync = true;
                    }
                    if keep_samples {
                        result.samples[ti].push(Mp4Sample {
                            data: Vec::new(),
                            source: Some((offset, sample.size)),
                            dts,
                            pts,
                            duration: sample.duration,
                            is_key,
                            offset: 0,
                        });
                    }
                    dts = dts
                        .checked_add(u64::from(sample.duration))
                        .ok_or_else(|| Error::invalid("decode timestamp overflow"))?;
                    pts.checked_add(i128::from(sample.duration))
                        .ok_or_else(|| Error::invalid("presentation timestamp overflow"))?;
                    offset = end;
                    result.sample_counts[ti] = result.sample_counts[ti]
                        .checked_add(1)
                        .ok_or_else(|| Error::invalid("sample count overflow"))?;
                    result.decode_ends[ti] = dts;
                    result.last_durations[ti] = sample.duration;
                    result.presentation_ends[ti] =
                        result.presentation_ends[ti].max(pts + i128::from(sample.duration));
                }
                Ok(())
            })?;
            if run_number == 0 {
                return Err(Error::invalid("missing trun"));
            }
            Ok(())
        })?;
        if seen.is_empty() {
            return Err(Error::invalid("moof does not contain a traf box"));
        }
        position = mdat.end;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mp4::{FragmentedMp4Muxer, FragmentedTrack};
    use std::cell::Cell;
    use std::io::Cursor;

    #[test]
    fn edited_fragments_restore_movie_time_but_index_media_time() {
        // Includes fractional movie milliseconds and a version-1 edit duration.
        for offset in [1, 66_179, u64::from(u32::MAX) + 1] {
            let mut mux = FragmentedMp4Muxer::new(vec![FragmentedTrack::audio(
                1,
                48000,
                2,
                vec![0x11, 0x90],
            )]);
            let mut data = mux.write_header_with_offsets(&[offset]).unwrap();
            for dts in [0, 1024] {
                data.extend(
                    mux.write_fragment(&[vec![Mp4Sample {
                        data: vec![1; 16],
                        source: None,
                        dts,
                        pts: i128::from(dts) - 1,
                        duration: 1024,
                        is_key: true,
                        offset: 0,
                    }]])
                    .unwrap(),
                );
            }
            let index = scan(
                &mut Cursor::new(&data),
                data.len() as u64,
                true,
                true,
                &|| Ok(()),
            )
            .unwrap();
            assert_eq!(index.samples[0][0].dts, offset);
            assert_eq!(index.samples[0][0].pts, i128::from(offset) - 1);
            assert_eq!(index.samples[0][1].dts, offset + 1024);
            assert_eq!(index.decode_ends[0], offset + 2048);
            assert_eq!(index.presentation_ends[0], i128::from(offset) + 2047);
            assert_eq!(index.entries[0][0].time, 0);
            assert_eq!(index.entries[0][1].time, 1024);
        }
    }

    fn fixture() -> Vec<u8> {
        let track = FragmentedTrack::audio(1, 48000, 2, vec![0x11, 0x90]);
        let mut mux = FragmentedMp4Muxer::new(vec![track]);
        let mut data = mux.write_header().unwrap();
        for dts in [0, 2048] {
            data.extend(
                mux.write_fragment(&[vec![
                    Mp4Sample {
                        data: vec![1; 64 * 1024],
                        source: None,
                        dts,
                        pts: i128::from(dts),
                        duration: 1024,
                        is_key: true,
                        offset: 0,
                    },
                    Mp4Sample {
                        data: vec![2; 64 * 1024],
                        source: None,
                        dts: dts + 1024,
                        pts: i128::from(dts) + 1024,
                        duration: 1024,
                        is_key: true,
                        offset: 0,
                    },
                ]])
                .unwrap(),
            );
        }
        data
    }

    struct Guarded {
        data: Cursor<Vec<u8>>,
        forbidden: Vec<std::ops::Range<u64>>,
        reads: usize,
        seeks: usize,
    }
    impl Read for Guarded {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            assert!(out.len() <= 8192);
            let start = self.data.position();
            let end = start + out.len() as u64;
            assert!(
                !self
                    .forbidden
                    .iter()
                    .any(|range| start < range.end && end > range.start),
                "scanner read media payload"
            );
            self.reads += out.len();
            self.data.read(out)
        }
    }
    impl Seek for Guarded {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            self.seeks += 1;
            self.data.seek(pos)
        }
    }

    #[test]
    fn scanner_skips_payload_and_preserves_sample_metadata() {
        let data = fixture();
        let mut cursor = Cursor::new(data.clone());
        let mut forbidden = Vec::new();
        let mut position = 0;
        while position < data.len() as u64 {
            let h = header(&mut cursor, position, data.len() as u64).unwrap();
            if h.kind == *b"mdat" {
                forbidden.push(h.payload..h.end);
            }
            position = h.end;
        }
        let mut reader = Guarded {
            data: Cursor::new(data),
            forbidden,
            reads: 0,
            seeks: 0,
        };
        let limit = reader.data.get_ref().len() as u64;
        let index = scan(&mut reader, limit, true, true, &|| Ok(())).unwrap();
        assert_eq!(index.fragments, 2);
        assert_eq!(index.samples[0].len(), 4);
        assert_eq!(index.samples[0][3].dts, 3072);
        assert_eq!(index.samples[0][3].duration, 1024);
        assert!(
            index.samples[0]
                .iter()
                .all(|s| s.data.is_empty() && s.source.unwrap().1 == 65536)
        );
        assert_eq!(index.entries[0].len(), 2);
        assert!(reader.reads < 2048);
        assert!(reader.seeks > 8);
        let recovery = scan(&mut reader, limit, false, true, &|| Ok(())).unwrap();
        assert!(recovery.samples[0].is_empty());
    }

    #[test]
    fn scanner_can_skip_index_without_losing_recovery_or_sample_metadata() {
        let data = fixture();
        let limit = data.len() as u64;
        let indexed = scan(&mut Cursor::new(&data), limit, true, true, &|| Ok(())).unwrap();
        for keep_samples in [false, true] {
            let index = scan(&mut Cursor::new(&data), limit, keep_samples, false, &|| {
                Ok(())
            })
            .unwrap();
            assert!(
                index
                    .entries
                    .iter()
                    .all(|track| track.is_empty() && track.capacity() == 0)
            );
            assert_eq!(index.fragments, indexed.fragments);
            assert_eq!(index.sample_counts, indexed.sample_counts);
            assert_eq!(index.decode_ends, indexed.decode_ends);
            assert_eq!(index.presentation_ends, indexed.presentation_ends);
            assert_eq!(index.last_durations, indexed.last_durations);
            assert_eq!(index.samples[0].len(), if keep_samples { 4 } else { 0 });
        }
        // Skipping retention must not skip checkpoint validation.
        assert!(scan(&mut Cursor::new(&data), limit - 1, false, false, &|| Ok(())).is_err());
    }

    #[test]
    fn invalid_boundaries_sequences_and_sample_ranges_are_rejected() {
        let original = fixture();
        for limit in [0, 7, original.len() as u64 - 1, original.len() as u64 + 1] {
            assert!(scan(&mut Cursor::new(&original), limit, true, true, &|| Ok(())).is_err());
        }
        for (tag, relative, value) in [
            (b"mfhd", 8, 99u32),
            (b"trun", 12, u32::MAX),
            (b"trun", 8, u32::MAX),
            (b"trun", 20, u32::MAX),
        ] {
            let mut data = original.clone();
            let pos = data.windows(4).position(|w| w == tag).unwrap() + relative;
            data[pos..pos + 4].copy_from_slice(&value.to_be_bytes());
            assert!(
                scan(
                    &mut Cursor::new(&data),
                    data.len() as u64,
                    true,
                    true,
                    &|| Ok(())
                )
                .is_err()
            );
        }
    }

    #[test]
    fn tail_is_ignored_only_outside_committed_prefix() {
        let data = fixture();
        for tail in [&b"half box"[..], &data[..]] {
            let mut appended = data.clone();
            appended.extend(tail);
            assert_eq!(
                scan(
                    &mut Cursor::new(appended),
                    data.len() as u64,
                    false,
                    true,
                    &|| Ok(())
                )
                .unwrap()
                .fragments,
                2
            );
        }
    }

    #[test]
    fn extended_size_boxes_and_cancel_are_checked() {
        let mut data = fixture();
        // An extended ftyp changes absolute offsets, while relative trun offsets stay valid.
        let size = u32::from_be_bytes(data[..4].try_into().unwrap()) as u64;
        data[..4].copy_from_slice(&1u32.to_be_bytes());
        data.splice(8..8, (size + 8).to_be_bytes());
        assert_eq!(
            scan(
                &mut Cursor::new(&data),
                data.len() as u64,
                true,
                true,
                &|| Ok(())
            )
            .unwrap()
            .fragments,
            2
        );
        let calls = Cell::new(0);
        let result = scan(
            &mut Cursor::new(&data),
            data.len() as u64,
            true,
            true,
            &|| {
                calls.set(calls.get() + 1);
                if calls.get() > 8 {
                    Err(Error::Cancelled)
                } else {
                    Ok(())
                }
            },
        );
        assert!(matches!(result, Err(Error::Cancelled)));
        let mut cursor = Cursor::new([
            0, 0, 0, 1, b'm', b'd', b'a', b't', 255, 255, 255, 255, 255, 255, 255, 255,
        ]);
        assert!(header(&mut cursor, 0, 16).is_err());
    }
    #[test]
    fn virtual_large_mdat_and_following_fragment_use_u64_offsets() {
        // A virtual sparse file: only metadata is stored; reads of the 4 GiB payload fail.
        struct VirtualFile {
            chunks: Vec<(u64, Vec<u8>)>,
            position: u64,
            length: u64,
        }
        impl Seek for VirtualFile {
            fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
                self.position = match from {
                    SeekFrom::Start(p) => p,
                    SeekFrom::End(p) => self.length.checked_add_signed(p).unwrap(),
                    SeekFrom::Current(p) => self.position.checked_add_signed(p).unwrap(),
                };
                Ok(self.position)
            }
        }
        impl Read for VirtualFile {
            fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                let (start, data) = self
                    .chunks
                    .iter()
                    .find(|(start, bytes)| {
                        self.position >= *start
                            && self.position + out.len() as u64 <= *start + bytes.len() as u64
                    })
                    .expect("scanner must not read virtual media payload");
                let local = (self.position - start) as usize;
                out.copy_from_slice(&data[local..local + out.len()]);
                self.position += out.len() as u64;
                Ok(out.len())
            }
        }
        let mut mux =
            FragmentedMp4Muxer::new(vec![FragmentedTrack::audio(1, 48000, 2, vec![0x11, 0x90])]);
        let mut first = mux.write_header().unwrap();
        let sample = |dts| Mp4Sample {
            data: vec![0],
            source: None,
            dts,
            pts: i128::from(dts),
            duration: 1024,
            is_key: true,
            offset: 0,
        };
        let mut fragment = mux.write_fragment(&[vec![sample(0)]]).unwrap();
        let trun = fragment.windows(4).position(|w| w == b"trun").unwrap();
        let offset = i32::from_be_bytes(fragment[trun + 12..trun + 16].try_into().unwrap());
        fragment[trun + 12..trun + 16].copy_from_slice(&(offset + 8).to_be_bytes());
        fragment[trun + 20..trun + 24].copy_from_slice(&u32::MAX.to_be_bytes());
        first.extend_from_slice(&fragment[..fragment.len() - 9]);
        first.extend_from_slice(&1u32.to_be_bytes());
        first.extend_from_slice(b"mdat");
        first.extend_from_slice(&(u64::from(u32::MAX) + 16).to_be_bytes());
        let second_start = first.len() as u64 + u64::from(u32::MAX);
        let second = mux.write_fragment(&[vec![sample(1024)]]).unwrap();
        let limit = second_start + second.len() as u64;
        let mut reader = VirtualFile {
            chunks: vec![(0, first), (second_start, second)],
            position: 0,
            length: limit,
        };
        let index = scan(&mut reader, limit, true, true, &|| Ok(())).unwrap();
        assert_eq!(index.fragments, 2);
        assert_eq!(index.samples[0][0].source.unwrap().1, u32::MAX);
        assert!(index.samples[0][1].source.unwrap().0 > u64::from(u32::MAX));
        assert!(index.entries[0][1].moof_offset > u64::from(u32::MAX));
    }

    /// Run in a separate process to measure peak RSS without earlier tests.
    #[test]
    #[ignore = "manual RSS and throughput benchmark"]
    fn finalize_benchmark() {
        use crate::mp4::Mp4Muxer;
        use std::fs::{File, OpenOptions};
        use std::io::Write;
        use std::time::Instant;
        let count: u32 = std::env::var("HLS_BENCH_SAMPLES")
            .unwrap_or_else(|_| "128".into())
            .parse()
            .unwrap();
        let size: u32 = std::env::var("HLS_BENCH_SAMPLE_BYTES")
            .unwrap_or_else(|_| "1048576".into())
            .parse()
            .unwrap();
        assert!(size > 0 && size <= u32::MAX - 8);
        let folder = std::env::temp_dir().join(format!("hls-bench-{}", std::process::id()));
        std::fs::create_dir(&folder).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(folder.clone());
        let source = folder.join("source.mp4");
        let target = folder.join("output.mp4");
        let track = FragmentedTrack::audio(1, 48000, 2, vec![0x11, 0x90]);
        let mut mux = FragmentedMp4Muxer::new(vec![track]);
        let mut file = File::create(&source).unwrap();
        file.write_all(&mux.write_header().unwrap()).unwrap();
        for i in 0..count {
            let sample = Mp4Sample {
                data: vec![0],
                source: None,
                dts: u64::from(i) * 1024,
                pts: i128::from(i) * 1024,
                duration: 1024,
                is_key: true,
                offset: 0,
            };
            let mut fragment = mux.write_fragment(&[vec![sample]]).unwrap();
            let trun = fragment.windows(4).position(|w| w == b"trun").unwrap();
            fragment[trun + 20..trun + 24].copy_from_slice(&size.to_be_bytes());
            let mdat = fragment.len() - 9;
            file.write_all(&fragment[..mdat]).unwrap();
            file.write_all(&(size + 8).to_be_bytes()).unwrap();
            file.write_all(b"mdat").unwrap();
            // Sparse input keeps setup memory and physical disk usage bounded.
            file.seek(SeekFrom::Current(i64::from(size))).unwrap();
        }
        let limit = file.stream_position().unwrap();
        file.set_len(limit).unwrap();
        drop(file);
        let mut input = File::open(&source).unwrap();
        let started = Instant::now();
        let index = scan(&mut input, limit, true, true, &|| Ok(())).unwrap();
        let scan_seconds = started.elapsed().as_secs_f64();
        let tracks = super::super::file_tracks(&index.init)
            .unwrap()
            .into_iter()
            .zip(index.samples)
            .map(|(t, samples)| t.into_classic(samples))
            .collect();
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&target)
            .unwrap();
        let copy_started = Instant::now();
        let (bytes, _) = Mp4Muxer::new(tracks)
            .write_file(&mut input, &mut output, &|| Ok(()))
            .unwrap();
        output.flush().unwrap();
        let copy_seconds = copy_started.elapsed().as_secs_f64();
        #[cfg(unix)]
        let physical = {
            use std::os::unix::fs::MetadataExt;
            (std::fs::metadata(&source).unwrap().blocks()
                + std::fs::metadata(&target).unwrap().blocks())
                * 512
        };
        #[cfg(not(unix))]
        let physical = 0;
        let finalize_seconds = scan_seconds + copy_seconds;
        println!(
            "BENCH {{\"samples\":{count},\"sample_bytes\":{size},\"input_bytes\":{limit},\"output_bytes\":{bytes},\"scan_seconds\":{scan_seconds},\"copy_seconds\":{copy_seconds},\"finalize_seconds\":{finalize_seconds},\"copy_mib_s\":{},\"logical_disk_bytes\":{},\"physical_disk_bytes\":{physical}}}",
            u64::from(count) as f64 * f64::from(size) / 1048576.0 / copy_seconds,
            limit + bytes
        );
    }
}

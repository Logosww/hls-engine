//! Native-only faststart finalization of private fMP4 temporary files.
use super::*;
use std::io::{Read, Seek, SeekFrom, Write};

/// Present the unmodified fMP4 after a private leading free box to the strict
/// scanner. Scanner offsets stay relative to that fMP4, including tfhd offsets.
/// Neither the scanner nor the WASM/recovery layouts need to change.
pub(crate) struct ReservedReader<'a> {
    file: &'a mut std::fs::File,
    base: u64,
}
impl<'a> ReservedReader<'a> {
    pub(crate) fn new(file: &'a mut std::fs::File, base: u64) -> Self {
        Self { file, base }
    }
}
impl Read for ReservedReader<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.file.read(bytes)
    }
}
impl Seek for ReservedReader<'_> {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        let position = match position {
            SeekFrom::Start(n) => SeekFrom::Start(
                n.checked_add(self.base)
                    .ok_or_else(|| std::io::Error::other("file offset overflow"))?,
            ),
            other => other,
        };
        self.file
            .seek(position)?
            .checked_sub(self.base)
            .ok_or_else(|| std::io::Error::other("seek before reserved prefix"))
    }
}

/// memmove for overlapping file ranges: read the last block before overwriting
/// it at its higher destination. Memory is bounded independently of file size.
fn shift_right<F: Read + Write + Seek>(
    file: &mut F,
    start: u64,
    end: u64,
    shift: u64,
    check: &dyn Fn() -> Result<()>,
) -> Result<()> {
    if shift == 0 {
        return Ok(());
    }
    end.checked_add(shift)
        .ok_or_else(|| Error::muxing("file shift overflow"))?;
    let mut buffer = vec![0; 1024 * 1024];
    let mut position = end;
    while position > start {
        check()?;
        let n = (position - start).min(buffer.len() as u64) as usize;
        position -= n as u64;
        file.seek(SeekFrom::Start(position))?;
        file.read_exact(&mut buffer[..n])?;
        file.seek(SeekFrom::Start(position + shift))?;
        file.write_all(&buffer[..n])?;
    }
    Ok(())
}

impl Mp4Muxer {
    fn in_place_moov(
        &mut self,
        reserve: u64,
        media_min: u64,
        limit: u64,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<(Vec<u8>, u64)> {
        let mut chunks = Vec::new();
        for track in &self.tracks {
            let mut track_chunks: Vec<ChunkMeta> = Vec::new();
            let mut previous_end = None;
            for (index, sample) in track.samples().iter().enumerate() {
                check()?;
                let (offset, size) = sample
                    .source
                    .ok_or_else(|| Error::muxing("missing sample source"))?;
                let end = offset
                    .checked_add(u64::from(size))
                    .filter(|end| offset >= media_min && *end <= limit)
                    .ok_or_else(|| Error::muxing("sample outside original file"))?;
                // Never merge samples across headers or another track's bytes.
                if previous_end != Some(offset) {
                    track_chunks.push(ChunkMeta {
                        first_sample: index,
                        sample_count: 0,
                    });
                }
                track_chunks.last_mut().unwrap().sample_count += 1;
                previous_end = Some(end);
            }
            chunks.push(track_chunks);
        }
        let mut front = reserve;
        loop {
            check()?;
            for track in &mut self.tracks {
                for sample in track.samples_mut() {
                    sample.offset = sample
                        .source
                        .unwrap()
                        .0
                        .checked_add(front)
                        .ok_or_else(|| Error::muxing("sample offset overflow"))?;
                }
            }
            let moov = moov_box(&self.tracks, &chunks, &self.metadata)?;
            let needed = moov.len() as u64;
            let mut next = front.max(needed);
            // A leftover 1..7 bytes cannot form a free box. Grow by at most 7
            // bytes instead of writing an invalid box or falling back to a tail moov.
            if next > needed && next - needed < 8 {
                next = needed
                    .checked_add(8)
                    .ok_or_else(|| Error::muxing("header size overflow"))?;
            }
            if next == front {
                return Ok((moov, front));
            }
            // Moving samples can promote stco to co64, growing moov again.
            // Offsets and table widths only grow, so this converges.
            front = next;
        }
    }

    pub(crate) fn finalize_in_place(
        mut self,
        file: &mut std::fs::File,
        init: &[u8],
        original_size: u64,
        reserve: u64,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<(u64, Vec<TrackInfo>)> {
        self.prepare_classic(check)?;
        let ftyp_size = init.get(..4).ok_or_else(|| Error::muxing("missing ftyp"))?;
        let ftyp_size = u64::from(u32::from_be_bytes(ftyp_size.try_into().unwrap()));
        let limit = original_size
            .checked_sub(reserve)
            .ok_or_else(|| Error::muxing("invalid reserved prefix"))?;
        if ftyp_size < 8
            || ftyp_size
                .checked_add(16)
                .is_none_or(|n| n > init.len() as u64)
            || init.len() as u64 > limit
            || file.metadata()?.len() != original_size
        {
            return Err(Error::muxing("invalid in-place initialization layout"));
        }
        let (moov, front) = self.in_place_moov(reserve, init.len() as u64, limit, check)?;
        let shift = front - reserve;
        let total = original_size
            .checked_add(shift)
            .ok_or_else(|| Error::muxing("output size overflow"))?;
        check()?;
        // All layout validation precedes mutation. Only the private temp is
        // touched; cancellation/I/O failure never publishes a partial result.
        if shift > 0 {
            file.set_len(total)?;
        }
        shift_right(file, reserve + ftyp_size, original_size, shift, check)?;
        check()?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&init[..ftyp_size as usize])?;
        for block in moov.chunks(64 * 1024) {
            check()?;
            file.write_all(block)?;
        }
        let remaining = front - moov.len() as u64;
        if remaining > 0 {
            let mut free = Vec::new();
            if let Ok(n) = u32::try_from(remaining) {
                be_u32(&mut free, n);
                free.extend_from_slice(b"free");
            } else {
                be_u32(&mut free, 1);
                free.extend_from_slice(b"free");
                be_u64(&mut free, remaining);
            }
            file.write_all(&free)?;
        }
        check()?;
        // Old initialization and fragment headers become unreferenced mdat bytes.
        let mut header = Vec::with_capacity(16);
        be_u32(&mut header, 1);
        header.extend_from_slice(b"mdat");
        be_u64(&mut header, limit - ftyp_size);
        file.seek(SeekFrom::Start(ftyp_size + front))?;
        file.write_all(&header)?;
        file.flush()?;
        check()?;
        Ok((
            total,
            self.tracks
                .iter()
                .filter_map(Mp4Track::track_info)
                .collect(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    fn scratch() -> (Scratch, std::fs::File) {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "hls-in-place-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let file = std::fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        (Scratch(path), file)
    }
    fn sample(dts: u64, value: u8) -> Mp4Sample {
        Mp4Sample {
            data: vec![value; 4],
            source: None,
            dts,
            pts: i128::from(dts),
            duration: 1024,
            is_key: true,
            offset: 0,
        }
    }
    fn fixture() -> Vec<u8> {
        let mut mux = FragmentedMp4Muxer::new(vec![
            FragmentedTrack::audio(1, 48000, 2, vec![0x11, 0x90]),
            FragmentedTrack::audio(2, 48000, 2, vec![0x11, 0x90]),
        ]);
        let mut bytes = mux.write_header().unwrap();
        for i in 0..3 {
            bytes.extend(
                mux.write_fragment(&[
                    vec![sample(i * 2048, 1), sample(i * 2048 + 1024, 2)],
                    vec![sample(i * 2048, 3), sample(i * 2048 + 1024, 4)],
                ])
                .unwrap(),
            );
        }
        bytes
    }
    fn indexed(file: &mut std::fs::File, reserve: u64, len: u64) -> (Mp4Muxer, Vec<u8>) {
        let scan = crate::isobmff::scan_file(
            &mut ReservedReader::new(file, reserve),
            len - reserve,
            true,
            false,
            &|| Ok(()),
        )
        .unwrap();
        (
            Mp4Muxer::from_fragments(
                crate::isobmff::file_tracks(&scan.init).unwrap(),
                scan.samples,
            ),
            scan.init,
        )
    }
    #[test]
    fn reservation_boundaries_preserve_inode_samples_and_chunk_boundaries() {
        let original = fixture();
        let (_path, mut file) = scratch();
        file.write_all(&original).unwrap();
        let (mut mux, init) = indexed(&mut file, 0, original.len() as u64);
        mux.prepare_classic(&|| Ok(())).unwrap();
        let (moov, _) = mux
            .in_place_moov(0, init.len() as u64, original.len() as u64, &|| Ok(()))
            .unwrap();
        let m = moov.len() as u64;
        for reserve in [
            0,
            8,
            m - 1,
            m,
            m + 1,
            m + 2,
            m + 3,
            m + 4,
            m + 5,
            m + 6,
            m + 7,
            m + 8,
            1024 * 1024,
        ] {
            let (path, mut file) = scratch();
            file.set_len(reserve).unwrap();
            file.seek(SeekFrom::Start(reserve)).unwrap();
            file.write_all(&original).unwrap();
            let old_size = reserve + original.len() as u64;
            let (mux, init) = indexed(&mut file, reserve, old_size);
            let expected: Vec<Vec<u64>> = mux
                .tracks
                .iter()
                .map(|t| {
                    t.samples()
                        .iter()
                        .step_by(2)
                        .map(|s| s.source.unwrap().0)
                        .collect()
                })
                .collect();
            #[cfg(unix)]
            let inode = {
                use std::os::unix::fs::MetadataExt;
                file.metadata().unwrap().ino()
            };
            let (size, reports) = mux
                .finalize_in_place(&mut file, &init, old_size, reserve, &|| Ok(()))
                .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                assert_eq!(inode, std::fs::metadata(&path.0).unwrap().ino());
            }
            let result = std::fs::read(&path.0).unwrap();
            assert_eq!(result.len() as u64, size);
            assert!(reports.iter().all(|t| t.sample_count == 6));
            let front = size - original.len() as u64;
            assert!(front >= reserve);
            if reserve == m || reserve >= m + 8 {
                assert_eq!(front, reserve);
            }
            let ftyp = u32::from_be_bytes(result[..4].try_into().unwrap()) as usize;
            assert_eq!(&result[ftyp + 4..ftyp + 8], b"moov");
            let moov_len = u32::from_be_bytes(result[ftyp..ftyp + 4].try_into().unwrap()) as usize;
            let padding = front - moov_len as u64;
            assert!(padding == 0 || padding >= 8);
            if padding > 0 {
                assert_eq!(&result[ftyp + moov_len + 4..ftyp + moov_len + 8], b"free");
            }
            assert_eq!(
                &result[ftyp + front as usize + 4..ftyp + front as usize + 8],
                b"mdat"
            );
            assert_eq!(
                &result[front as usize + init.len()..],
                &original[init.len()..]
            );
            let moov = &result[ftyp..ftyp + moov_len];
            let offsets: Vec<Vec<u64>> = moov
                .windows(4)
                .enumerate()
                .filter(|(_, b)| *b == b"stco")
                .map(|(p, _)| {
                    let n = u32::from_be_bytes(moov[p + 8..p + 12].try_into().unwrap()) as usize;
                    moov[p + 12..p + 12 + n * 4]
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|b| u64::from(u32::from_be_bytes(*b)))
                        .collect()
                })
                .collect();
            assert_eq!(
                offsets,
                expected
                    .iter()
                    .map(|t| t.iter().map(|n| n + front).collect::<Vec<_>>())
                    .collect::<Vec<_>>()
            );
        }
    }
    #[test]
    fn relocation_handles_overlap_block_boundaries_cancellation_and_io_error() {
        use std::io::Cursor;
        let original: Vec<u8> = (0..(2 * 1024 * 1024 + 17))
            .map(|n| (n % 251) as u8)
            .collect();
        for shift in [0, 1, 7, 1024 * 1024 + 31] {
            let mut file = Cursor::new(original.clone());
            shift_right(&mut file, 11, original.len() as u64, shift, &|| Ok(())).unwrap();
            assert_eq!(&file.get_ref()[11 + shift as usize..], &original[11..]);
        }
        let mut file = Cursor::new(original.clone());
        let checks = std::cell::Cell::new(0);
        assert!(matches!(
            shift_right(&mut file, 0, original.len() as u64, 1, &|| {
                checks.set(checks.get() + 1);
                if checks.get() == 2 {
                    Err(Error::Cancelled)
                } else {
                    Ok(())
                }
            }),
            Err(Error::Cancelled)
        ));
        // A short source read must fail, never fabricate media bytes.
        assert!(shift_right(&mut Cursor::new(vec![1; 10]), 0, 20, 3, &|| Ok(())).is_err());
    }
    #[test]
    fn offset_promotion_replans_moov_and_checks_overflow() {
        let track = FragmentedTrack::audio(1, 48000, 2, vec![0x11, 0x90]);
        let mut s = sample(0, 42);
        s.data.clear();
        s.source = Some((u64::from(u32::MAX) - 4, 4));
        let mut mux = Mp4Muxer::from_fragments(vec![track], vec![vec![s]]);
        mux.prepare_classic(&|| Ok(())).unwrap();
        let (moov, front) = mux
            .in_place_moov(0, 0, u64::from(u32::MAX), &|| Ok(()))
            .unwrap();
        let p = moov.windows(4).position(|b| b == b"co64").unwrap();
        assert_eq!(
            u64::from_be_bytes(moov[p + 12..p + 20].try_into().unwrap()),
            u64::from(u32::MAX) - 4 + front
        );
        assert!(front >= moov.len() as u64);
        assert!(
            mux.in_place_moov(u64::MAX, 0, u64::from(u32::MAX), &|| Ok(()))
                .is_err()
        );
    }
    // Unix holes avoid allocating 4 GiB. Offset promotion is tested on all targets.
    #[cfg(unix)]
    #[test]
    fn sparse_large_file_needs_no_media_copy_when_reserved_space_fits() {
        use std::io::Read;

        let (_path, mut file) = scratch();
        let reserve = 4096;
        let track = FragmentedTrack::audio(1, 48000, 2, vec![0x11, 0x90]);
        let init = FragmentedMp4Muxer::new(vec![track.clone()])
            .write_header()
            .unwrap();
        file.seek(SeekFrom::Start(reserve)).unwrap();
        file.write_all(&init).unwrap();
        let offset = u64::from(u32::MAX) + 128;
        file.seek(SeekFrom::Start(reserve + offset)).unwrap();
        file.write_all(&[42; 4]).unwrap();
        let mut s = sample(0, 42);
        s.data.clear();
        s.source = Some((offset, 4));
        let mux = Mp4Muxer::from_fragments(vec![track], vec![vec![s]]);
        let (size, _) = mux
            .finalize_in_place(&mut file, &init, reserve + offset + 4, reserve, &|| Ok(()))
            .unwrap();
        assert_eq!(size, reserve + offset + 4);
        file.seek(SeekFrom::Start(reserve + offset)).unwrap();
        let mut payload = [0; 4];
        file.read_exact(&mut payload).unwrap();
        assert_eq!(payload, [42; 4]);
    }
    #[test]
    fn cancellation_before_mutation_preserves_file() {
        let (path, mut file) = scratch();
        let bytes = fixture();
        file.write_all(&bytes).unwrap();
        let (mux, init) = indexed(&mut file, 0, bytes.len() as u64);
        assert!(matches!(
            mux.finalize_in_place(&mut file, &init, bytes.len() as u64, 0, &|| Err(
                Error::Cancelled
            )),
            Err(Error::Cancelled)
        ));
        assert_eq!(std::fs::read(&path.0).unwrap(), bytes);
    }
}

#[cfg(test)]
#[path = "in_place_benchmark.rs"]
mod benchmark;

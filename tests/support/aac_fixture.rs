//! Generated synthetic transport packets; no external media.
fn ts_packet(pid: u16, payload: &[u8], cc: u8) -> Vec<u8> {
    assert!(payload.len() <= 182);
    let mut p = vec![0xff; 188];
    p[..4].copy_from_slice(&[0x47, 0x40 | (pid >> 8) as u8, pid as u8, 0x30 | (cc & 15)]);
    let n = 183 - payload.len();
    p[4] = n as u8;
    if n > 0 {
        p[5] = 0;
    }
    p[5 + n..].copy_from_slice(payload);
    p
}
pub fn segment(times: &[u64]) -> Vec<u8> {
    segment_with_rate(times, 3)
}
pub fn segment_with_rate(times: &[u64], rate_index: u8) -> Vec<u8> {
    let mut b = ts_packet(
        0,
        &[
            0, 0, 0xb0, 13, 0, 1, 0xc1, 0, 0, 0, 1, 0xe0, 0x20, 0, 0, 0, 0,
        ],
        0,
    );
    b.extend(ts_packet(
        32,
        &[
            0, 2, 0xb0, 18, 0, 1, 0xc1, 0, 0, 0xe1, 0, 0xf0, 0, 0x0f, 0xe1, 0, 0xf0, 0, 0, 0, 0, 0,
        ],
        0,
    ));
    for (i, &t) in times.iter().enumerate() {
        let mut pes = vec![
            0,
            0,
            1,
            0xc0,
            0,
            16,
            0x80,
            0x80,
            5,
            0x21 | (((t >> 30) & 7) as u8) << 1,
            (t >> 22) as u8,
            (((t >> 15) & 127) as u8) << 1 | 1,
            (t >> 7) as u8,
            ((t & 127) as u8) << 1 | 1,
        ];
        // ADTS AAC-LC, stereo, 48 kHz, one raw byte: parsing/mux test, not decoder audio.
        pes.extend([
            0xff,
            0xf1,
            0x40 | (rate_index << 2),
            0x80,
            1,
            0x1f,
            0xfc,
            i as u8,
        ]);
        b.extend(ts_packet(256, &pes, i as u8));
    }
    b
}

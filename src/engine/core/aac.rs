//! Bounded, content-preserving repair. Source clocks never use repaired timestamps.
use super::*;
use crate::state_codec::state_struct;
use std::time::Duration;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum AacTimestampPolicy {
    #[default]
    Strict,
    RepairOverlaps {
        max_shift: Duration,
    },
}
impl AacTimestampPolicy {
    pub(super) fn enabled(self) -> bool {
        !matches!(self, Self::Strict)
    }
    pub(super) fn limit(self) -> EngineResult<Option<u64>> {
        match self {
            Self::Strict => Ok(None),
            Self::RepairOverlaps { max_shift } => {
                let n = u64::try_from(max_shift.as_nanos())
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| fail(EngineErrorKind::InvalidOptions))?;
                Ok(Some(n))
            }
        }
    }
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct AacRepairPosition {
    pub slot: SegmentSlot,
    pub original: MediaTime,
    pub repaired: MediaTime,
}
#[derive(Debug, Clone)]
pub struct AacTimestampRepairError {
    pub position: AacRepairPosition,
    pub max_shift: Duration,
}
/// An overlap rejected by Strict on a selected MPEG-TS AAC-LC track.
/// Times are in the unwrapped source clock, before presentation mapping.
/// This local overlap does not guarantee that a whole-input repair will fit a limit.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct AacTimestampOverlapError {
    pub slot: SegmentSlot,
    pub original: MediaTime,
    /// Earliest next frame time at the validation boundary that rejected the frame.
    pub expected: MediaTime,
    /// `expected - original`, including the preceding frame's duration.
    pub overlap: MediaTime,
}

pub(super) fn overlap_error(
    slot: &SegmentSlot,
    original: MediaTime,
    expected: MediaTime,
) -> EngineResult<EngineError> {
    let mut error = at_slot(EngineErrorKind::AacTimestampOverlap, slot);
    error.aac_overlap = Some(Box::new(AacTimestampOverlapError {
        slot: slot.clone(),
        original,
        expected,
        overlap: sub(expected, original)?,
    }));
    Ok(error)
}
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct AacRepairSummary {
    pub input_id: InputId,
    #[cfg_attr(feature = "serde", serde(serialize_with = "decimal"))]
    pub corrected_frames: u64,
    pub max_shift: MediaTime,
    pub first: Option<AacRepairPosition>,
    pub last: Option<AacRepairPosition>,
}
#[cfg(feature = "serde")]
fn decimal<S: serde::Serializer>(v: &u64, s: S) -> std::result::Result<S::Ok, S::Error> {
    s.serialize_str(&v.to_string())
}
impl AacRepairSummary {
    fn new(input_id: InputId) -> Self {
        Self {
            input_id,
            corrected_frames: 0,
            max_shift: zero(),
            first: None,
            last: None,
        }
    }
    fn record(&mut self, p: AacRepairPosition, shift: MediaTime) -> EngineResult<()> {
        self.corrected_frames = self
            .corrected_frames
            .checked_add(1)
            .ok_or_else(|| fail(EngineErrorKind::TimeOverflow))?;
        self.max_shift = max(self.max_shift, shift)?;
        if self.first.is_none() {
            self.first = Some(p.clone());
        }
        self.last = Some(p);
        Ok(())
    }
}
#[derive(Clone, Default)]
pub(super) struct AacRepairState {
    pub next: Option<MediaTime>,
    pub summary: Option<AacRepairSummary>,
}
impl AacRepairState {
    pub fn apply(
        &mut self,
        packet: &mut EncodedPacket,
        rate: u32,
        slot: &SegmentSlot,
        limit_ns: u64,
    ) -> EngineResult<Option<AacRepairPosition>> {
        let old = packet.timing.ok_or_else(|| fail(EngineErrorKind::Media))?;
        if rate == 0 || packet.duration != 1024 || old.pts != old.dts || old.edit_offset != 0 {
            return Err(at_slot(EngineErrorKind::Media, slot));
        }
        let original = MediaTime::new(old.dts, old.timescale).map_err(media_error)?;
        let repaired = self.next.map_or(Ok(original), |next| max(original, next))?;
        let shift = sub(repaired, original)?;
        let limit = MediaTime {
            ticks: i128::from(limit_ns),
            timescale: 1_000_000_000,
        };
        if cmp(shift, limit)?.is_gt() {
            let mut e = at_slot(EngineErrorKind::AacTimestampRepairLimitExceeded, slot);
            e.aac = Some(Box::new(AacTimestampRepairError {
                position: AacRepairPosition {
                    slot: slot.clone(),
                    original,
                    repaired,
                },
                max_shift: Duration::from_nanos(limit_ns),
            }));
            return Err(e);
        }
        // LCM retains the original PES anchor and an exact number of audio samples.
        let mut a = old.timescale;
        let mut b = rate;
        while b != 0 {
            (a, b) = (b, a % b);
        }
        let timescale = (old.timescale / a)
            .checked_mul(rate)
            .ok_or_else(|| fail(EngineErrorKind::TimeOverflow))?;
        let dts = scale(repaired, timescale)?;
        let duration = 1024u32
            .checked_mul(timescale / rate)
            .ok_or_else(|| fail(EngineErrorKind::TimeOverflow))?;
        packet.timing = Some(PacketTiming {
            dts,
            pts: dts,
            duration,
            timescale,
            edit_offset: 0,
        });
        self.next = Some(add(
            repaired,
            MediaTime {
                ticks: 1024,
                timescale: rate,
            },
        )?);
        let summary = self
            .summary
            .get_or_insert_with(|| AacRepairSummary::new(slot.input_id().clone()));
        if shift.ticks > 0 {
            let p = AacRepairPosition {
                slot: slot.clone(),
                original,
                repaired,
            };
            summary.record(p.clone(), shift)?;
            Ok(Some(p))
        } else {
            Ok(None)
        }
    }
}
fn at_slot(kind: EngineErrorKind, slot: &SegmentSlot) -> EngineError {
    EngineError {
        slot: Some(slot.clone()),
        ..fail(kind)
    }
}
state_struct!(AacRepairPosition {
    slot,
    original,
    repaired
});
state_struct!(AacRepairSummary {
    input_id,
    corrected_frames,
    max_shift,
    first,
    last
});
impl crate::state_codec::StateCodec for AacRepairState {
    fn put(&self, out: &mut Vec<u8>) {
        self.next.put(out);
        self.summary.put(out);
    }
    fn get(r: &mut crate::state_codec::Reader<'_>) -> crate::state_codec::DecodeResult<Self> {
        use crate::state_codec::StateCodec;
        let next: Option<MediaTime> = StateCodec::get(r)?;
        let summary: Option<AacRepairSummary> = StateCodec::get(r)?;
        if next.is_some_and(|t| t.timescale == 0) || (next.is_some() && summary.is_none()) {
            return Err(());
        }
        if let Some(s) = &summary {
            if s.max_shift.timescale == 0
                || s.max_shift.ticks < 0
                || s.first.is_none() != s.last.is_none()
                || (s.corrected_frames == 0) != (s.first.is_none() && s.last.is_none())
            {
                return Err(());
            }
            for p in [&s.first, &s.last].into_iter().flatten() {
                if p.original.timescale == 0
                    || p.repaired.timescale == 0
                    || p.slot.input_id() != &s.input_id
                    || cmp(p.repaired, p.original).map_err(|_| ())?.is_lt()
                {
                    return Err(());
                }
            }
        }
        Ok(Self { next, summary })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn slot() -> SegmentSlot {
        let snap = crate::parse_playlist_snapshot(
            &crate::TextResource {
                content: "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1,\na.ts\n#EXT-X-ENDLIST\n"
                    .into(),
                location: crate::SourceLocation::Url(url::Url::parse("https://test/list").unwrap()),
            },
            crate::playlist::PlaylistContext::new(InputId::new("audio").unwrap(), 0),
        )
        .unwrap();
        snap.segments()[0].slot().clone()
    }
    fn packet(t: i128) -> EncodedPacket {
        EncodedPacket {
            kind: StreamKind::Aac,
            data: vec![1, 2, 3],
            timing: Some(PacketTiming {
                dts: t,
                pts: t,
                timescale: 90_000,
                duration: 1920,
                edit_offset: 0,
            }),
            pts_90k: t,
            dts_90k: t as u64,
            duration: 1024,
            is_key: true,
            is_length_prefixed: false,
        }
    }
    #[test]
    fn overlap_limits_payload_and_source_clock() {
        for limit in [40, 44, 50] {
            let mut state = AacRepairState::default();
            let slot = slot();
            state
                .apply(&mut packet(26880), 48000, &slot, limit * 1_000_000)
                .unwrap();
            let mut p = packet(24840);
            let result = state.apply(&mut p, 48000, &slot, limit * 1_000_000);
            if limit == 40 {
                assert_eq!(
                    result.unwrap_err().kind(),
                    EngineErrorKind::AacTimestampRepairLimitExceeded
                );
                continue;
            }
            assert_eq!(
                result.unwrap().unwrap().repaired,
                MediaTime {
                    ticks: 8,
                    timescale: 25
                }
            );
            assert_eq!(p.data, [1, 2, 3]);
            assert_eq!(p.dts_90k, 24840);
            assert_eq!(p.timing.unwrap().dts, 230400);
            assert_eq!(state.summary.unwrap().corrected_frames, 1);
        }
    }
    #[test]
    fn cumulative_overlap_and_forward_gap() {
        let mut s = AacRepairState::default();
        let slot = slot();
        s.apply(&mut packet(0), 48000, &slot, 50_000_000).unwrap();
        s.apply(&mut packet(0), 48000, &slot, 50_000_000).unwrap();
        s.apply(&mut packet(0), 48000, &slot, 50_000_000).unwrap();
        assert_eq!(
            s.apply(&mut packet(0), 48000, &slot, 50_000_000)
                .unwrap_err()
                .kind(),
            EngineErrorKind::AacTimestampRepairLimitExceeded
        );
        let mut p = packet(90_000);
        assert!(s.apply(&mut p, 48000, &slot, 50_000_000).unwrap().is_none());
        assert_eq!(
            packet_time(&p, 0),
            MediaTime {
                ticks: 720000,
                timescale: 720000
            }
        );
    }
    #[test]
    fn exact_sample_clocks_survive_checkpoint() {
        use crate::state_codec::{Reader, StateCodec};
        let slot = slot();
        for rate in [44_100, 48_000] {
            let mut s = AacRepairState::default();
            for i in 0..100_000i128 {
                let mut p = packet(i * 1024 * 90000 / i128::from(rate));
                s.apply(&mut p, rate, &slot, 50_000_000).unwrap();
                assert_eq!(scale(packet_time(&p, 0), rate).unwrap(), i * 1024);
                if i == 50_000 {
                    let mut bytes = vec![];
                    s.put(&mut bytes);
                    s = AacRepairState::get(&mut Reader(&bytes)).unwrap();
                }
            }
            assert_eq!(scale(s.next.unwrap(), rate).unwrap(), 100_000 * 1024);
        }
    }
}

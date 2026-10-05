//! Internal two-stage boundary. No sample-encryption capability is exported.
use crate::{Error, Result, types::StreamKind};
use sha2::{Digest, Sha256};
use std::{borrow::Cow, collections::HashMap, future::Future, pin::Pin};

mod protection;
use protection::Protection;

#[derive(Clone, Copy)]
pub(crate) enum RawLayout {
    Fragment { offset: u64, prefix: usize },
    AnnexB,
    Adts { offset: usize },
}
pub(crate) struct RawSample {
    pub kind: StreamKind,
    pub layout: RawLayout,
    pub dts: i128,
    pub timescale: u32,
    pub bytes: Vec<u8>,
    pub protection: Option<Protection>,
}
impl RawSample {
    fn identity(&self) -> [u8; 32] {
        identity(
            self.kind,
            self.layout,
            self.dts,
            self.timescale,
            &self.bytes,
            self.protection.as_ref(),
        )
    }
}
fn identity(
    kind: StreamKind,
    layout: RawLayout,
    dts: i128,
    scale: u32,
    bytes: &[u8],
    protection: Option<&Protection>,
) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update([match kind {
        StreamKind::Avc => 0,
        StreamKind::Hevc => 1,
        StreamKind::Aac => 2,
    }]);
    match layout {
        RawLayout::Fragment { offset, prefix } => {
            hash.update([0]);
            hash.update(offset.to_be_bytes());
            hash.update((prefix as u64).to_be_bytes());
        }
        RawLayout::AnnexB => hash.update([1]),
        RawLayout::Adts { offset } => {
            hash.update([2]);
            hash.update((offset as u64).to_be_bytes());
        }
    }
    hash.update(dts.to_be_bytes());
    hash.update(scale.to_be_bytes());
    hash.update([u8::from(protection.is_some())]);
    if let Some(protection) = protection {
        protection.hash(&mut hash);
    }
    hash.update(bytes);
    hash.finalize().into()
}
pub(crate) trait RawSampleHook {
    fn process<'a>(
        &'a mut self,
        sample: RawSample,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>>> + 'a>>;
}
pub(crate) struct ClearSamples;
impl RawSampleHook for ClearSamples {
    fn process<'a>(
        &'a mut self,
        sample: RawSample,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>>> + 'a>> {
        Box::pin(async move {
            if sample.protection.is_some() {
                return Err(Error::unsupported("sample protection is not enabled"));
            }
            Ok(sample.bytes)
        })
    }
}
pub(crate) enum RawStage {
    Clear,
    Collect(Vec<RawSample>),
    Replay(HashMap<[u8; 32], Vec<u8>>),
}
impl RawStage {
    pub(crate) fn sample<'a>(
        &mut self,
        kind: StreamKind,
        layout: RawLayout,
        dts: i128,
        scale: u32,
        bytes: &'a [u8],
    ) -> Result<Option<Cow<'a, [u8]>>> {
        self.protected_sample(kind, layout, dts, scale, bytes, None)
    }
    #[allow(clippy::too_many_arguments)]
    fn protected_sample<'a>(
        &mut self,
        kind: StreamKind,
        layout: RawLayout,
        dts: i128,
        scale: u32,
        bytes: &'a [u8],
        protection: Option<&Protection>,
    ) -> Result<Option<Cow<'a, [u8]>>> {
        if let Some(protection) = protection {
            protection.validate(bytes.len())?;
        }
        match self {
            Self::Clear if protection.is_none() => Ok(Some(Cow::Borrowed(bytes))),
            Self::Clear => Err(Error::unsupported("sample protection is not enabled")),
            Self::Collect(samples) => {
                samples.push(RawSample {
                    kind,
                    layout,
                    dts,
                    timescale: scale,
                    bytes: bytes.to_vec(),
                    protection: protection.cloned(),
                });
                Ok(None)
            }
            Self::Replay(samples) => {
                let data = samples
                    .get(&identity(kind, layout, dts, scale, bytes, protection))
                    .ok_or_else(|| Error::bitstream("raw sample identity changed"))?;
                if data.len() != bytes.len() {
                    return Err(Error::bitstream("raw hook changed protected byte layout"));
                }
                Ok(Some(Cow::Owned(data.clone())))
            }
        }
    }
    pub(crate) async fn resolve(
        self,
        hook: &mut dyn RawSampleHook,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Self> {
        let Self::Collect(samples) = self else {
            return Err(Error::invalid("invalid raw stage"));
        };
        let mut clear = HashMap::new();
        for sample in samples {
            check()?;
            if let Some(protection) = &sample.protection {
                protection.validate(sample.bytes.len())?;
            }
            let id = sample.identity();
            let length = sample.bytes.len();
            let protected = sample.protection.clone().map(|p| (p, sample.bytes.clone()));
            let bytes = hook.process(sample).await?;
            check()?;
            if bytes.len() != length {
                return Err(Error::bitstream("raw hook changed protected byte layout"));
            }
            if let Some((protection, original)) = protected {
                protection.check_clear_bytes(&original, &bytes)?;
            }
            clear.insert(id, bytes);
        }
        Ok(Self::Replay(clear))
    }
}

#[cfg(test)]
mod packed;

/// Packed AAC prototype: decode only the specified ID3 PRIV clock, never EXTINF.
#[cfg(test)]
fn id3_timestamp(owner: &str, bytes: &[u8]) -> Result<u64> {
    if owner != "com.apple.streaming.transportStreamTimestamp" || bytes.len() != 8 {
        return Err(Error::bitstream("invalid ID3 PRIV timestamp"));
    }
    let value = u64::from_be_bytes(bytes.try_into().unwrap());
    if value >= 1 << 33 {
        return Err(Error::bitstream("ID3 clock exceeds 33 bits"));
    }
    Ok(value)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn id3_clock_is_exact_and_validated() {
        let owner = "com.apple.streaming.transportStreamTimestamp";
        assert_eq!(
            id3_timestamp(owner, &((1u64 << 33) - 1).to_be_bytes()).unwrap(),
            (1u64 << 33) - 1
        );
        assert!(id3_timestamp(owner, &(1u64 << 33).to_be_bytes()).is_err());
        assert!(id3_timestamp("other", &0u64.to_be_bytes()).is_err());
    }
    #[tokio::test]
    async fn asynchronous_hook_precedes_nal_normalization() {
        struct Pending {
            seen: usize,
        }
        impl RawSampleHook for Pending {
            fn process<'a>(
                &'a mut self,
                sample: RawSample,
            ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>>> + 'a>> {
                Box::pin(async move {
                    tokio::task::yield_now().await;
                    self.seen += 1;
                    Ok(sample.bytes)
                })
            }
        }
        let init = include_bytes!("../tests/fixtures/media/fmp4_avc_nal2_multirun/init.fmp4");
        let segment = include_bytes!("../tests/fixtures/media/fmp4_avc_nal2_multirun/seg0.m4s");
        let expected = crate::isobmff::demux_isobmff(init, segment).unwrap();
        let mut hook = Pending { seen: 0 };
        let actual = crate::isobmff::demux_isobmff_with_hook(init, segment, &mut hook, &|| Ok(()))
            .await
            .unwrap();
        assert_eq!(hook.seen, expected.packets.len());
        for (a, b) in actual.packets.iter().zip(expected.packets) {
            assert_eq!(a.data, b.data);
        }
        let ts = include_bytes!("../tests/fixtures/media/ts_avc_regular/seg0.ts");
        let expected = crate::mpeg_ts::demux_ts(ts).unwrap();
        let actual = crate::mpeg_ts::demux_ts_with_hook(ts, &mut hook, &|| Ok(()))
            .await
            .unwrap();
        assert_eq!(actual.packets.len(), expected.packets.len());
        let mut a: Vec<_> = actual
            .packets
            .iter()
            .map(|p| Sha256::digest(&p.data).to_vec())
            .collect();
        let mut b: Vec<_> = expected
            .packets
            .iter()
            .map(|p| Sha256::digest(&p.data).to_vec())
            .collect();
        a.sort();
        b.sort();
        assert_eq!(a, b);
    }

    #[tokio::test]
    async fn hook_can_restore_raw_nals_before_clear_codec_validation() {
        struct Capture(HashMap<u64, Vec<u8>>);
        impl RawSampleHook for Capture {
            fn process<'a>(
                &'a mut self,
                sample: RawSample,
            ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>>> + 'a>> {
                Box::pin(async move {
                    tokio::task::yield_now().await;
                    if !matches!(sample.kind, StreamKind::Aac)
                        && let RawLayout::Fragment { offset, prefix } = sample.layout
                    {
                        assert_eq!(prefix, 2);
                        if let Some(original) = self.0.get(&offset) {
                            return Ok(original.clone());
                        }
                        self.0.insert(offset, sample.bytes.clone());
                    }
                    Ok(sample.bytes)
                })
            }
        }
        let init = include_bytes!("../tests/fixtures/media/fmp4_avc_nal2_multirun/init.fmp4");
        let original = include_bytes!("../tests/fixtures/media/fmp4_avc_nal2_multirun/seg0.m4s");
        let mut hook = Capture(HashMap::new());
        let expected =
            crate::isobmff::demux_isobmff_with_hook(init, original, &mut hook, &|| Ok(()))
                .await
                .unwrap();
        assert!(!hook.0.is_empty());
        let mut protected = original.to_vec();
        for offset in hook.0.keys() {
            let start = *offset as usize;
            protected[start..start + 2].fill(0xff);
        }
        assert!(crate::isobmff::demux_isobmff(init, &protected).is_err());
        let restored =
            crate::isobmff::demux_isobmff_with_hook(init, &protected, &mut hook, &|| Ok(()))
                .await
                .unwrap();
        for (before, after) in expected.packets.iter().zip(&restored.packets) {
            assert_eq!(before.data, after.data);
        }
        assert_eq!(expected.packets.len(), restored.packets.len());
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    struct Identity;
    impl RawSampleHook for Identity {
        fn process<'a>(
            &'a mut self,
            sample: RawSample,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>>> + 'a>> {
            Box::pin(async move { Ok(sample.bytes) })
        }
    }

    #[tokio::test]
    async fn replay_binds_original_offset_prefix_clock_and_kind() {
        for prefix in 1..=4 {
            let layout = RawLayout::Fragment {
                offset: 123,
                prefix,
            };
            let bytes = [0, 0, 0, 2, 0x65, 0x88];
            let mut stage = RawStage::Collect(Vec::new());
            assert!(
                stage
                    .sample(StreamKind::Avc, layout, -17, 90_000, &bytes)
                    .unwrap()
                    .is_none()
            );
            let mut replay = stage.resolve(&mut Identity, &|| Ok(())).await.unwrap();
            assert_eq!(
                replay
                    .sample(StreamKind::Avc, layout, -17, 90_000, &bytes)
                    .unwrap()
                    .unwrap()
                    .as_ref(),
                bytes
            );
            for (kind, layout, clock, scale) in [
                (StreamKind::Hevc, layout, -17, 90_000),
                (
                    StreamKind::Avc,
                    RawLayout::Fragment {
                        offset: 124,
                        prefix,
                    },
                    -17,
                    90_000,
                ),
                (
                    StreamKind::Avc,
                    RawLayout::Fragment {
                        offset: 123,
                        prefix: prefix + 1,
                    },
                    -17,
                    90_000,
                ),
                (StreamKind::Avc, RawLayout::AnnexB, -17, 90_000),
                (
                    StreamKind::Avc,
                    RawLayout::Adts { offset: 123 },
                    -17,
                    90_000,
                ),
                (StreamKind::Avc, layout, -16, 90_000),
                (StreamKind::Avc, layout, -17, 48_000),
            ] {
                assert!(replay.sample(kind, layout, clock, scale, &bytes).is_err());
            }
            assert!(
                replay
                    .sample(StreamKind::Avc, layout, -17, 90_000, &[9; 6])
                    .is_err()
            );
        }
    }

    #[tokio::test]
    async fn raw_hook_length_changes_and_cancellation_fail_before_replay() {
        struct Resize;
        impl RawSampleHook for Resize {
            fn process<'a>(
                &'a mut self,
                mut sample: RawSample,
            ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>>> + 'a>> {
                Box::pin(async move {
                    sample.bytes.push(0);
                    Ok(sample.bytes)
                })
            }
        }
        fn collected() -> RawStage {
            RawStage::Collect(vec![RawSample {
                kind: StreamKind::Aac,
                layout: RawLayout::Adts { offset: 7 },
                dts: 123,
                timescale: 48_000,
                bytes: vec![1, 2, 3],
                protection: None,
            }])
        }
        assert!(collected().resolve(&mut Resize, &|| Ok(())).await.is_err());
        let calls = std::cell::Cell::new(0);
        let result = collected()
            .resolve(&mut Identity, &|| {
                calls.set(calls.get() + 1);
                if calls.get() == 2 {
                    Err(Error::Cancelled)
                } else {
                    Ok(())
                }
            })
            .await;
        assert!(matches!(result, Err(Error::Cancelled)));
        assert_eq!(calls.get(), 2);
    }
}

//! Validated internal hook descriptors. Container parsing and sample decryption
//! remain disabled; these types specify the byte contract for the future reader.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)] // Reserved internal hook variants, not advertised capabilities.
pub(crate) enum Scheme {
    Cenc,
    Cbcs { crypt: u8, skip: u8 },
}
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum Iv {
    Sample(Vec<u8>),
    Constant([u8; 16]),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Subsample {
    pub clear: u16,
    pub encrypted: u32,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Protection {
    pub scheme: Scheme,
    pub kid: [u8; 16],
    pub iv: Iv,
    /// Empty means the entire original sample; otherwise exact full coverage.
    pub subsamples: Vec<Subsample>,
}
impl Protection {
    pub fn validate(&self, size: usize) -> Result<()> {
        let invalid = || Error::bitstream("invalid raw protection descriptor");
        match (&self.scheme, &self.iv) {
            (Scheme::Cenc, Iv::Sample(iv)) if matches!(iv.len(), 8 | 16) => {}
            (Scheme::Cbcs { crypt, skip }, iv) if *crypt <= 15 && *skip <= 15 => {
                if matches!(iv, Iv::Sample(bytes) if bytes.len() != 16) {
                    return Err(invalid());
                }
            }
            _ => return Err(invalid()),
        }
        if !self.subsamples.is_empty() {
            let total = self.subsamples.iter().try_fold(0usize, |total, part| {
                total
                    .checked_add(usize::from(part.clear))
                    .and_then(|n| n.checked_add(usize::try_from(part.encrypted).ok()?))
                    .filter(|n| *n <= size)
                    .ok_or_else(invalid)
            })?;
            if total != size {
                return Err(invalid());
            }
        }
        Ok(())
    }
    pub fn hash(&self, hash: &mut Sha256) {
        match self.scheme {
            Scheme::Cenc => hash.update([0, 0, 0]),
            Scheme::Cbcs { crypt, skip } => hash.update([1, crypt, skip]),
        }
        hash.update(self.kid);
        match &self.iv {
            Iv::Sample(iv) => {
                hash.update([0, iv.len() as u8]);
                hash.update(iv);
            }
            Iv::Constant(iv) => {
                hash.update([1, 16]);
                hash.update(iv);
            }
        }
        hash.update((self.subsamples.len() as u64).to_be_bytes());
        for part in &self.subsamples {
            hash.update(part.clear.to_be_bytes());
            hash.update(part.encrypted.to_be_bytes());
        }
    }
    pub fn check_clear_bytes(&self, before: &[u8], after: &[u8]) -> Result<()> {
        self.validate(before.len())?;
        if before.len() != after.len() {
            return Err(Error::bitstream("raw hook changed protected byte layout"));
        }
        let mut offset = 0;
        if self.subsamples.is_empty() {
            return self.check_pattern(before, after);
        }
        for part in &self.subsamples {
            let end = offset + usize::from(part.clear);
            if before[offset..end] != after[offset..end] {
                return Err(Error::bitstream("raw hook changed clear subsample bytes"));
            }
            offset = end + part.encrypted as usize;
            self.check_pattern(&before[end..offset], &after[end..offset])?;
        }
        Ok(())
    }
    fn check_pattern(&self, before: &[u8], after: &[u8]) -> Result<()> {
        let Scheme::Cbcs { crypt, skip } = self.scheme else {
            return Ok(());
        };
        // Match the unpatterned CBCS 0:0 convention used by Shaka Packager:
        // https://github.com/shaka-project/shaka-packager/blob/main/packager/media/base/aes_pattern_cryptor.cc
        let crypt = usize::from(if crypt == 0 && skip == 0 { 1 } else { crypt }) * 16;
        let skip = usize::from(skip) * 16;
        let mut cursor = 0;
        while cursor < before.len() {
            // Only complete encrypted blocks can change. Partial CBC tails and
            // skipped pattern blocks are clear, including within a subsample.
            cursor += (before.len() - cursor).min(crypt) / 16 * 16;
            let end = if before.len() - cursor < 16 {
                before.len()
            } else {
                (cursor + skip).min(before.len())
            };
            if before[cursor..end] != after[cursor..end] {
                return Err(Error::bitstream("raw hook changed clear pattern bytes"));
            }
            cursor = end;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn descriptor() -> Protection {
        Protection {
            scheme: Scheme::Cenc,
            kid: [3; 16],
            iv: Iv::Sample(vec![7; 16]),
            subsamples: vec![
                Subsample {
                    clear: 2,
                    encrypted: 4,
                },
                Subsample {
                    clear: 1,
                    encrypted: 3,
                },
            ],
        }
    }
    #[test]
    fn descriptors_validate_iv_pattern_and_exact_original_sample_coverage() {
        let original = descriptor();
        original.validate(10).unwrap();
        assert!(original.validate(9).is_err());
        assert!(original.validate(11).is_err());
        for length in [0, 1, 7, 8, 15, 16, 17] {
            let mut value = original.clone();
            value.scheme = Scheme::Cbcs { crypt: 1, skip: 9 };
            value.iv = Iv::Sample(vec![0; length]);
            assert_eq!(value.validate(10).is_ok(), length == 16);
            value.scheme = Scheme::Cenc;
            assert_eq!(value.validate(10).is_ok(), matches!(length, 8 | 16));
        }
        let mut value = original.clone();
        value.iv = Iv::Constant([7; 16]);
        assert!(value.validate(10).is_err()); // CTR cannot silently inherit a constant IV.
        for (crypt, skip) in [(16, 1), (1, 16)] {
            let mut value = original.clone();
            value.scheme = Scheme::Cbcs { crypt, skip };
            assert!(value.validate(10).is_err());
        }
        let mut value = original.clone();
        value.subsamples = vec![Subsample {
            clear: u16::MAX,
            encrypted: u32::MAX,
        }];
        assert!(value.validate(10).is_err());
        value.subsamples.clear();
        value.validate(10).unwrap(); // Whole sample.
    }
    struct Restore {
        corrupt_clear: bool,
    }
    impl RawSampleHook for Restore {
        fn process<'a>(&'a mut self, mut sample: RawSample) -> RawFuture<'a> {
            Box::pin(async move {
                tokio::task::yield_now().await;
                assert_eq!(sample.protection.as_ref(), Some(&descriptor()));
                for index in [2, 3, 4, 5, 7, 8, 9] {
                    sample.bytes[index] ^= 0xff;
                }
                if self.corrupt_clear {
                    sample.bytes[0] ^= 1;
                }
                Ok(sample.bytes)
            })
        }
    }
    #[tokio::test]
    async fn protected_hook_binds_descriptor_and_preserves_clear_prefixes() {
        let bytes = [0, 4, 1, 2, 3, 4, 0x65, 5, 6, 7];
        let layout = RawLayout::Fragment {
            track: 1,
            offset: 81,
            prefix: 2,
        };
        let protection = descriptor();
        for corrupt_clear in [false, true] {
            let mut stage = RawStage::Collect(Vec::new());
            stage
                .protected_sample(
                    StreamKind::Avc,
                    layout,
                    -9,
                    90_000,
                    &bytes,
                    Some(&protection),
                )
                .unwrap();
            let result = stage
                .resolve(&mut Restore { corrupt_clear }, &|| Ok(()))
                .await;
            if corrupt_clear {
                assert!(result.is_err());
                continue;
            }
            let mut replay = result.unwrap();
            let clear = replay
                .protected_sample(
                    StreamKind::Avc,
                    layout,
                    -9,
                    90_000,
                    &bytes,
                    Some(&protection),
                )
                .unwrap()
                .unwrap();
            assert_eq!(&clear[..2], &bytes[..2]);
            assert_eq!(clear[2], bytes[2] ^ 0xff);
            assert_eq!(clear[6], bytes[6]);
            let mut variants = vec![protection.clone(); 5];
            variants[0].kid[0] ^= 1;
            variants[1].iv = Iv::Sample(vec![7; 8]);
            variants[2].scheme = Scheme::Cbcs { crypt: 2, skip: 8 };
            variants[3].subsamples[0] = Subsample {
                clear: 3,
                encrypted: 3,
            };
            variants[4].iv = Iv::Sample(vec![8; 16]);
            for changed in variants {
                assert!(
                    replay
                        .protected_sample(
                            StreamKind::Avc,
                            layout,
                            -9,
                            90_000,
                            &bytes,
                            Some(&changed)
                        )
                        .is_err()
                );
            }
            assert!(
                replay
                    .sample(StreamKind::Avc, layout, -9, 90_000, &bytes)
                    .is_err()
            );
        }
    }

    #[tokio::test]
    async fn invalid_protection_and_missing_decryptor_fail_closed() {
        let bytes = [0; 10];
        let layout = RawLayout::Fragment {
            track: 1,
            offset: 8,
            prefix: 4,
        };
        let mut protection = descriptor();
        protection.subsamples[0].encrypted += 1;
        let mut stage = RawStage::Collect(Vec::new());
        assert!(
            stage
                .protected_sample(
                    StreamKind::Avc,
                    layout,
                    0,
                    90_000,
                    &bytes,
                    Some(&protection)
                )
                .is_err()
        );
        assert!(matches!(&stage, RawStage::Collect(samples) if samples.is_empty()));
        protection = descriptor();
        assert!(
            RawStage::Clear
                .protected_sample(
                    StreamKind::Avc,
                    layout,
                    0,
                    90_000,
                    &bytes,
                    Some(&protection)
                )
                .is_err()
        );
        stage
            .protected_sample(
                StreamKind::Avc,
                layout,
                0,
                90_000,
                &bytes,
                Some(&protection),
            )
            .unwrap();
        assert!(stage.resolve(&mut ClearSamples, &|| Ok(())).await.is_err());
    }

    #[test]
    fn cbcs_skipped_blocks_and_partial_tails_remain_clear_per_subsample() {
        let mut value = Protection {
            scheme: Scheme::Cbcs { crypt: 1, skip: 1 },
            kid: [0; 16],
            iv: Iv::Constant([0; 16]),
            subsamples: vec![
                Subsample {
                    clear: 2,
                    encrypted: 36,
                },
                Subsample {
                    clear: 1,
                    encrypted: 35,
                },
            ],
        };
        let original = vec![0; 74];
        let mut restored = original.clone();
        restored[2..18].fill(1);
        restored[39..55].fill(2);
        value.check_clear_bytes(&original, &restored).unwrap();
        for index in [0, 1, 18, 33, 34, 37, 38, 55, 70, 71, 73] {
            let mut invalid = restored.clone();
            invalid[index] = 3;
            assert!(
                value.check_clear_bytes(&original, &invalid).is_err(),
                "{index}"
            );
        }
        value.subsamples.clear();
        for (crypt, skip) in [(0, 0), (1, 0), (5, 5)] {
            value.scheme = Scheme::Cbcs { crypt, skip };
            value.validate(31).unwrap();
            let mut after = [0; 31];
            after[..16].fill(1);
            value.check_clear_bytes(&[0; 31], &after).unwrap();
            after[30] = 1;
            assert!(value.check_clear_bytes(&[0; 31], &after).is_err());
        }
        value.scheme = Scheme::Cbcs { crypt: 0, skip: 9 };
        value.check_clear_bytes(&original, &original).unwrap();
        assert!(value.check_clear_bytes(&original, &restored).is_err());
    }
}

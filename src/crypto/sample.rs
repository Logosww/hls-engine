//! Sample encryption diagnostics and shared demux boundary.
#![doc = include_str!("../../docs/sample-encryption.md")]
use super::{key::*, resource::*};
use crate::{
    Error,
    playlist::EncryptionMethod,
    raw_sample::{
        RawLayout, RawSample, RawSampleHook,
        protection::{Iv, Protection, Scheme},
    },
    types::{DemuxOutput, StreamKind},
};
use aes::cipher::{BlockDecryptMut, KeyIvInit, StreamCipher};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SampleErrorKind {
    Unsupported,
    InvalidMetadata,
    Key,
    Decrypt,
    Media,
    BudgetExceeded,
    Cancelled,
}
/// Default formatting excludes provider errors, key bytes and resource credentials.
pub struct SampleError {
    kind: SampleErrorKind,
    resource: Box<KeyResource>,
    sample: Option<usize>,
    track: Option<u32>,
    scheme: Option<&'static str>,
    key: Option<Box<KeyError>>,
    cause: Option<Box<Error>>,
    version: Option<KeyVersion>,
    reference: Option<Box<crate::playlist::KeyReference>>,
}
impl SampleError {
    pub fn kind(&self) -> SampleErrorKind {
        self.kind
    }
    pub fn resource(&self) -> &KeyResource {
        &self.resource
    }
    pub fn sample_index(&self) -> Option<usize> {
        self.sample
    }
    pub fn track_id(&self) -> Option<u32> {
        self.track
    }
    pub fn scheme(&self) -> Option<&str> {
        self.scheme
    }
    pub fn key_error(&self) -> Option<&KeyError> {
        self.key.as_deref()
    }
    pub fn raw_cause(&self) -> Option<&Error> {
        self.cause.as_deref()
    }
    pub fn key_version(&self) -> Option<&KeyVersion> {
        self.version.as_ref()
    }
    pub fn key_reference(&self) -> Option<&crate::playlist::KeyReference> {
        self.reference.as_deref()
    }
    pub fn method(&self) -> Option<&EncryptionMethod> {
        self.resource
            .keys()
            .candidates()
            .first()
            .map(|k| k.method())
    }
    fn new(resource: &KeyResource, cause: Error) -> Self {
        let kind = match &cause {
            Error::Cancelled => SampleErrorKind::Cancelled,
            Error::Unsupported(s) if s.contains("budget") => SampleErrorKind::BudgetExceeded,
            Error::Unsupported(_) => SampleErrorKind::Unsupported,
            Error::Bitstream(_) | Error::InvalidInput(_) => SampleErrorKind::InvalidMetadata,
            _ => SampleErrorKind::Media,
        };
        Self {
            kind,
            resource: Box::new(resource.clone()),
            sample: None,
            track: None,
            scheme: None,
            key: None,
            cause: Some(Box::new(cause)),
            version: None,
            reference: None,
        }
    }
}
impl fmt::Debug for SampleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SampleError")
            .field("kind", &self.kind)
            .field("resource", &self.resource)
            .field("sample", &self.sample)
            .field("track", &self.track)
            .field("scheme", &self.scheme)
            .finish_non_exhaustive()
    }
}
impl fmt::Display for SampleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sample {:?}", self.kind)
    }
}
impl std::error::Error for SampleError {}

pub(crate) async fn demux(
    session: &ResourceSession,
    request: &ResourceRequest,
    init: Option<&[u8]>,
    bytes: &[u8],
    check: &crate::raw_sample::SampleCheck<'_>,
) -> Result<DemuxOutput, SampleError> {
    demux_selected(session, request, init, bytes, check, false).await
}

pub(crate) async fn demux_selected(
    session: &ResourceSession,
    request: &ResourceRequest,
    init: Option<&[u8]>,
    bytes: &[u8],
    check: &crate::raw_sample::SampleCheck<'_>,
    packed: bool,
) -> Result<DemuxOutput, SampleError> {
    let packed = packed && (bytes.starts_with(b"ID3") || bytes.first() == Some(&0xff));
    if packed && init.is_some() {
        return Err(SampleError::new(
            request.resource(),
            Error::unsupported("Packed AAC cannot use MAP"),
        ));
    }
    let protected = request
        .resource()
        .keys()
        .candidates()
        .first()
        .is_some_and(|k| {
            matches!(
                k.method(),
                EncryptionMethod::SampleAes | EncryptionMethod::SampleAesCtr
            )
        });
    if !protected {
        let result = if packed {
            crate::raw_sample::packed::demux(bytes, None, check).await
        } else if let Some(init) = init {
            crate::isobmff::demux_isobmff_checked(init, bytes, check)
        } else {
            crate::mpeg_ts::demux_ts_checked(bytes, check)
        };
        return result.map_err(|e| SampleError::new(request.resource(), e));
    }
    let _permit = if protected {
        Some(session.reserve_samples(bytes.len()).map_err(|cause| {
            let mut error = SampleError::new(
                request.resource(),
                Error::unsupported("sample waiting budget exceeded"),
            );
            if cause.kind() == ResourceErrorKind::Cancelled {
                error = SampleError::new(request.resource(), Error::Cancelled);
            }
            error
        })?)
    } else {
        None
    };
    let mut hook = Decryptor {
        session,
        resource: request.resource().clone(),
        check,
        index: 0,
        resolved: false,
        error: None,
    };
    let result = if packed {
        crate::raw_sample::packed::demux(bytes, Some(&mut hook), check).await
    } else if let Some(init) = init {
        crate::isobmff::demux_isobmff_with_hook(init, bytes, &mut hook, check).await
    } else {
        crate::mpeg_ts::demux_ts_with_hook(bytes, &mut hook, check).await
    };
    result.map_err(|e| {
        hook.error.unwrap_or_else(|| {
            let mut error = SampleError::new(request.resource(), e);
            if hook.resolved && error.kind != SampleErrorKind::Cancelled {
                error.kind = SampleErrorKind::Media;
            }
            error
        })
    })
}
struct Decryptor<'a> {
    session: &'a ResourceSession,
    resource: KeyResource,
    check: &'a crate::raw_sample::SampleCheck<'a>,
    index: usize,
    resolved: bool,
    error: Option<SampleError>,
}
impl RawSampleHook for Decryptor<'_> {
    fn buffers(&mut self, raw: usize, replay: usize) {
        self.session.observe_sample_buffers(raw, replay);
    }
    fn resolved(&mut self) {
        self.resolved = true;
    }
    fn allows_annexb_resize(&self) -> bool {
        true
    }
    fn sample_aes_ts(&self) -> bool {
        self.resource
            .keys()
            .candidates()
            .first()
            .is_some_and(|k| matches!(k.method(), EncryptionMethod::SampleAes))
    }
    fn limits(&self) -> (usize, usize) {
        self.session.sample_limits()
    }
    fn process<'a>(&'a mut self, mut sample: RawSample) -> crate::raw_sample::RawFuture<'a> {
        Box::pin(async move {
            let index = self.index;
            self.index += 1;
            let result = async {
                (self.check)()?;
                let method = self
                    .resource
                    .keys()
                    .candidates()
                    .first()
                    .map(|k| k.method());
                let mut resource = self.resource.clone();
                if let Some(p) = &sample.protection {
                    let expected = if matches!(p.scheme, Scheme::Cenc) {
                        EncryptionMethod::SampleAesCtr
                    } else {
                        EncryptionMethod::SampleAes
                    };
                    if method != Some(&expected) {
                        return Err(Error::invalid("KEY method conflicts with sample scheme"));
                    }
                    resource = resource.with_kid(p.kid);
                } else if matches!(sample.layout, RawLayout::Fragment { .. })
                    || !matches!(
                        method,
                        Some(EncryptionMethod::SampleAes | EncryptionMethod::SampleAesCtr)
                    )
                {
                    return Ok(std::mem::take(&mut sample.bytes));
                } else if method == Some(&EncryptionMethod::SampleAesCtr)
                    || sample.kind == StreamKind::Hevc
                {
                    return Err(Error::unsupported(
                        "unsupported TS sample encryption codec or method",
                    ));
                }
                let key = self
                    .session
                    .resolve_sample_key(resource.clone())
                    .await
                    .map_err(|key| {
                        let mut error = SampleError::new(
                            &resource,
                            Error::invalid("sample key resolution failed"),
                        );
                        error.kind = if key.kind() == KeyErrorKind::Cancelled {
                            SampleErrorKind::Cancelled
                        } else {
                            SampleErrorKind::Key
                        };
                        error.key = Some(Box::new(key));
                        self.error = Some(error);
                        Error::invalid("sample key resolution failed")
                    })?;
                let mut context =
                    SampleError::new(&resource, Error::invalid("sample decryption failed"));
                context.version = Some(key.version().clone());
                context.reference = Some(Box::new(key.reference().clone()));
                self.error = Some(context);
                if let Some(p) = &sample.protection {
                    decrypt_common(&mut sample.bytes, p, key.secret().expose(), self.check)?;
                } else {
                    let iv = key
                        .reference()
                        .explicit_iv()
                        .unwrap_or_else(|| sequence_iv(resource.slot().sequence()));
                    match sample.layout {
                        RawLayout::AnnexB | RawLayout::Transport { adts: false, .. } => {
                            sample.bytes =
                                decrypt_avc(&sample.bytes, key.secret().expose(), &iv, self.check)?
                        }
                        RawLayout::Adts { .. } | RawLayout::Transport { adts: true, .. } => {
                            if sample.bytes.len() > 16 {
                                cbc_pattern(
                                    &mut sample.bytes[16..],
                                    key.secret().expose(),
                                    &iv,
                                    0,
                                    0,
                                    self.check,
                                )?;
                            }
                        }
                        RawLayout::Fragment { .. } => unreachable!(),
                    }
                }
                (self.check)()?;
                self.error = None;
                Ok(std::mem::take(&mut sample.bytes))
            }
            .await;
            if let Err(cause) = result {
                let mut error = self.error.take().unwrap_or_else(|| {
                    SampleError::new(&self.resource, Error::invalid("sample processing failed"))
                });
                if error.key.is_none() {
                    error.kind = SampleError::new(&self.resource, Error::invalid("invalid")).kind;
                    if matches!(cause, Error::Cancelled) {
                        error.kind = SampleErrorKind::Cancelled;
                    } else if matches!(cause, Error::Unsupported(_)) {
                        error.kind = SampleErrorKind::Unsupported;
                    }
                }
                error.track = match sample.layout {
                    RawLayout::Transport { pid, .. } => Some(u32::from(pid)),
                    RawLayout::Fragment { track, .. } => Some(track),
                    _ => None,
                };
                error.sample = Some(index);
                error.scheme = sample.protection.as_ref().map(|p| {
                    if matches!(p.scheme, Scheme::Cenc) {
                        "cenc"
                    } else {
                        "cbcs"
                    }
                });
                error.cause = Some(Box::new(cause));
                self.error = Some(error);
                Err(Error::invalid("sample processing failed"))
            } else {
                result
            }
        })
    }
}
fn decrypt_common(
    bytes: &mut [u8],
    p: &Protection,
    key: &[u8],
    check: &dyn Fn() -> crate::Result<()>,
) -> crate::Result<()> {
    p.validate(bytes.len())?;
    let mut iv = [0; 16];
    match &p.iv {
        Iv::Sample(v) => iv[..v.len()].copy_from_slice(v),
        Iv::Constant(v) => iv = *v,
    }
    let ranges = if p.subsamples.is_empty() {
        vec![(0, bytes.len())]
    } else {
        let mut pos = 0;
        p.subsamples
            .iter()
            .map(|s| {
                pos += usize::from(s.clear);
                let start = pos;
                pos += s.encrypted as usize;
                (start, pos)
            })
            .collect()
    };
    match p.scheme {
        Scheme::Cenc => {
            let mut cipher = ctr::Ctr128BE::<aes::Aes128>::new_from_slices(key, &iv)
                .map_err(|_| Error::invalid("invalid AES key or IV"))?;
            for (start, end) in ranges {
                for part in bytes[start..end].chunks_mut(16 * 1024) {
                    check()?;
                    cipher
                        .try_apply_keystream(part)
                        .map_err(|_| Error::bitstream("CTR counter exhausted"))?;
                }
            }
        }
        Scheme::Cbcs { crypt, skip } => {
            // ISO CBCS restarts its IV and pattern at each protected subsample.
            for (start, end) in ranges {
                cbc_pattern(&mut bytes[start..end], key, &iv, crypt, skip, check)?;
            }
        }
    }
    Ok(())
}
fn cbc_pattern(
    bytes: &mut [u8],
    key: &[u8],
    iv: &[u8; 16],
    crypt: u8,
    skip: u8,
    check: &dyn Fn() -> crate::Result<()>,
) -> crate::Result<()> {
    let mut cipher = cbc::Decryptor::<aes::Aes128>::new_from_slices(key, iv)
        .map_err(|_| Error::invalid("invalid AES key or IV"))?;
    let (crypt, skip) = if crypt == 0 && skip == 0 {
        (1, 0)
    } else {
        (usize::from(crypt), usize::from(skip))
    };
    for (i, block) in bytes.as_chunks_mut::<16>().0.iter_mut().enumerate() {
        check()?;
        if i % (crypt + skip) < crypt {
            cipher.decrypt_block_mut(block.into());
        }
    }
    Ok(())
}
fn decrypt_avc(
    bytes: &[u8],
    key: &[u8],
    iv: &[u8; 16],
    check: &dyn Fn() -> crate::Result<()>,
) -> crate::Result<Vec<u8>> {
    let units = crate::codecs::avc::nal_units_annex_b(bytes);
    let mut output = Vec::with_capacity(bytes.len());
    let mut previous = 0;
    for unit in units {
        check()?;
        let start = unit.data.as_ptr() as usize - bytes.as_ptr() as usize;
        output.extend_from_slice(&bytes[previous..start]);
        if matches!(unit.data[0] & 31, 1 | 5) && unit.data.len() > 48 {
            let mut nal = crate::codecs::avc::remove_emulation_prevention(unit.data);
            let mut cipher = cbc::Decryptor::<aes::Aes128>::new_from_slices(key, iv)
                .map_err(|_| Error::invalid("invalid AES key or IV"))?;
            let mut pos = 32;
            while pos + 16 < nal.len() {
                check()?;
                cipher.decrypt_block_mut((&mut nal[pos..pos + 16]).into());
                pos += 160;
            }
            output.extend_from_slice(&nal);
        } else {
            output.extend_from_slice(unit.data);
        }
        previous = start + unit.data.len();
    }
    output.extend_from_slice(&bytes[previous..]);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raw_sample::protection::Subsample;
    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }
    #[test]
    fn nist_sp800_38a_ctr_and_subsample_counter_continuity() {
        let key = hex("2b7e151628aed2a6abf7158809cf4f3c");
        let iv = hex("f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff");
        let encrypted = hex(
            "874d6191b620e3261bef6864990db6ce9806f66b7970fdff8617187bb9fffdff5ae4df3edbd5d35e5b4f09020db03eab1e031dda2fbe03d1792170a0f3009cee",
        );
        let plain = hex(
            "6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e5130c81c46a35ce411e5fbc1191a0a52eff69f2445df4f9b17ad2b417be66c3710",
        );
        let mut p = Protection {
            scheme: Scheme::Cenc,
            kid: [0; 16],
            iv: Iv::Sample(iv),
            subsamples: vec![],
        };
        let mut bytes = encrypted.clone();
        decrypt_common(&mut bytes, &p, &key, &|| Ok(())).unwrap();
        assert_eq!(bytes, plain);
        let mut bytes = vec![9, 8];
        bytes.extend_from_slice(&encrypted[..7]);
        bytes.push(6);
        bytes.extend_from_slice(&encrypted[7..]);
        p.subsamples = vec![
            Subsample {
                clear: 2,
                encrypted: 7,
            },
            Subsample {
                clear: 1,
                encrypted: 57,
            },
        ];
        decrypt_common(&mut bytes, &p, &key, &|| Ok(())).unwrap();
        assert_eq!(&bytes[..2], &[9, 8]);
        assert_eq!(&bytes[2..9], &plain[..7]);
        assert_eq!(bytes[9], 6);
        assert_eq!(&bytes[10..], &plain[7..]);
    }
    #[test]
    fn nist_sp800_38a_cbc_pattern_and_subsample_reset() {
        let key = hex("2b7e151628aed2a6abf7158809cf4f3c");
        let iv: [u8; 16] = hex("000102030405060708090a0b0c0d0e0f").try_into().unwrap();
        let encrypted = hex("7649abac8119b246cee98e9b12e9197d5086cb9b507219ee95db113a917678b2");
        let plain = hex("6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e51");
        let mut bytes = encrypted[..16].to_vec();
        bytes.extend_from_slice(&[7; 16]);
        bytes.extend_from_slice(&encrypted[16..]);
        bytes.extend_from_slice(&[8; 3]);
        cbc_pattern(&mut bytes, &key, &iv, 1, 1, &|| Ok(())).unwrap();
        assert_eq!(&bytes[..16], &plain[..16]);
        assert_eq!(&bytes[16..32], &[7; 16]);
        assert_eq!(&bytes[32..48], &plain[16..]);
        assert_eq!(&bytes[48..], &[8; 3]);
        let mut bytes = encrypted[..16].repeat(2);
        let p = Protection {
            scheme: Scheme::Cbcs { crypt: 0, skip: 0 },
            kid: [0; 16],
            iv: Iv::Constant(iv),
            subsamples: vec![
                Subsample {
                    clear: 0,
                    encrypted: 16
                };
                2
            ],
        };
        decrypt_common(&mut bytes, &p, &key, &|| Ok(())).unwrap();
        assert_eq!(bytes, plain[..16].repeat(2));
        assert!(matches!(
            decrypt_common(&mut bytes, &p, &key, &|| Err(Error::Cancelled)),
            Err(Error::Cancelled)
        ));
    }
    #[test]
    fn apple_avc_boundaries_pattern_nal_reset_and_emulation_layer() {
        let key = hex("2b7e151628aed2a6abf7158809cf4f3c");
        let iv = hex("000102030405060708090a0b0c0d0e0f").try_into().unwrap();
        let ciphertext = hex("7649abac8119b246cee98e9b12e9197d5086cb9b507219ee95db113a917678b2");
        let plaintext = hex("6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e51");
        let mut nal = vec![0x55; 209];
        nal[0] = 0x65;
        nal[1..7].copy_from_slice(&[0, 0, 3, 0, 0, 1]);
        nal[32..48].copy_from_slice(&ciphertext[..16]);
        nal[192..208].copy_from_slice(&ciphertext[16..]);
        let mut clear = nal.clone();
        clear[32..48].copy_from_slice(&plaintext[..16]);
        clear[192..208].copy_from_slice(&plaintext[16..]);
        let escape = |nal: &[u8]| {
            let mut result = Vec::new();
            let mut zeros = 0;
            for &b in nal {
                if zeros == 2 && b <= 3 {
                    result.push(3);
                    zeros = 0;
                }
                result.push(b);
                zeros = if b == 0 { zeros + 1 } else { 0 };
            }
            result
        };
        let framed = |nal: &[u8]| [vec![0, 0, 0, 1], nal.to_vec()].concat();
        // Each NAL restarts CBC, but skipped blocks do not reset the CBC chain.
        let encrypted = framed(&escape(&nal)).repeat(2);
        assert_eq!(
            decrypt_avc(&encrypted, &key, &iv, &|| Ok(())).unwrap(),
            framed(&clear).repeat(2)
        );
        for size in [31, 32, 47, 48, 49] {
            let mut nal = vec![0x55; size];
            nal[0] = 0x61;
            if size >= 48 {
                nal[32..48].copy_from_slice(&ciphertext[..16]);
            }
            let mut expected = nal.clone();
            if size > 48 {
                expected[32..48].copy_from_slice(&plaintext[..16]);
            }
            assert_eq!(
                decrypt_avc(&framed(&nal), &key, &iv, &|| Ok(())).unwrap(),
                framed(&expected)
            );
        }
    }
}

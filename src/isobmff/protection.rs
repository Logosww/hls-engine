//! ISO common-encryption metadata. All offsets refer to the original resource.
use super::*;
use crate::raw_sample::protection::{Iv, Protection, Scheme, Subsample};

#[derive(Clone)]
pub(super) struct Defaults {
    scheme: Scheme,
    encrypted: bool,
    iv_size: usize,
    kid: [u8; 16],
    constant: Option<[u8; 16]>,
}
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}
impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|n| *n <= self.data.len())
            .ok_or_else(|| Error::bitstream("truncated protection metadata"))?;
        let v = &self.data[self.pos..end];
        self.pos = end;
        Ok(v)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn full(&mut self, versions: &[u8], allowed: u32) -> Result<(u8, u32)> {
        let n = self.u32()?;
        let version = (n >> 24) as u8;
        let flags = n & 0xffffff;
        if !versions.contains(&version) || flags & !allowed != 0 {
            return Err(Error::unsupported(
                "unsupported protection box version or flags",
            ));
        }
        Ok((version, flags))
    }
    fn end(&self) -> Result<()> {
        if self.pos != self.data.len() {
            return Err(Error::bitstream("trailing protection metadata"));
        }
        Ok(())
    }
}
fn unique<'a>(data: &'a [u8], name: &[u8; 4]) -> Result<Option<&'a [u8]>> {
    let mut count = 0;
    for_each_box(data, |k, _| {
        if k == name {
            count += 1;
        }
        Ok(())
    })?;
    if count > 1 {
        return Err(Error::bitstream("duplicate protection box"));
    }
    find_box(data, name)
}
fn required<'a>(data: &'a [u8], name: &[u8; 4]) -> Result<&'a [u8]> {
    unique(data, name)?.ok_or_else(|| Error::bitstream("missing protection box"))
}
fn descriptor(r: &mut Reader<'_>, scheme: &Scheme, pattern: u8) -> Result<Defaults> {
    let encrypted = match r.u8()? {
        0 => false,
        1 => true,
        _ => return Err(Error::bitstream("invalid isProtected")),
    };
    let iv_size = usize::from(r.u8()?);
    let kid = r.take(16)?.try_into().unwrap();
    let scheme = match scheme {
        Scheme::Cenc if pattern == 0 => Scheme::Cenc,
        Scheme::Cenc => return Err(Error::bitstream("cenc cannot use a pattern")),
        Scheme::Cbcs { .. } => Scheme::Cbcs {
            crypt: pattern >> 4,
            skip: pattern & 15,
        },
    };
    let constant = if encrypted && iv_size == 0 {
        if !matches!(scheme, Scheme::Cbcs { .. }) || r.u8()? != 16 {
            return Err(Error::bitstream("invalid constant IV"));
        }
        Some(r.take(16)?.try_into().unwrap())
    } else {
        None
    };
    if encrypted
        && !(matches!(scheme, Scheme::Cenc) && matches!(iv_size, 8 | 16)
            || matches!(scheme, Scheme::Cbcs { .. }) && (iv_size == 16 || constant.is_some()))
        || !encrypted && iv_size != 0
    {
        return Err(Error::bitstream("invalid protection IV size"));
    }
    Ok(Defaults {
        scheme,
        encrypted,
        iv_size,
        kid,
        constant,
    })
}
pub(super) fn entry(payload: &[u8], audio: bool) -> Result<([u8; 4], Defaults)> {
    let children = payload
        .get(if audio { 28.. } else { 78.. })
        .ok_or_else(|| Error::bitstream("short protected entry"))?;
    let sinf = required(children, b"sinf")?;
    let frma: [u8; 4] = required(sinf, b"frma")?
        .try_into()
        .map_err(|_| Error::bitstream("invalid frma"))?;
    if audio != (frma == *b"mp4a") {
        return Err(Error::bitstream("protected entry codec mismatch"));
    }
    let mut schm = Reader::new(required(sinf, b"schm")?);
    schm.full(&[0], 0)?;
    let scheme = match schm.take(4)? {
        b"cenc" => Scheme::Cenc,
        b"cbcs" => Scheme::Cbcs { crypt: 0, skip: 0 },
        _ => return Err(Error::unsupported("unknown encryption scheme")),
    };
    if schm.u32()? != 0x10000 {
        return Err(Error::unsupported("unknown encryption scheme version"));
    }
    schm.end()?;
    let mut tenc = Reader::new(required(required(sinf, b"schi")?, b"tenc")?);
    let (version, _) = tenc.full(&[0, 1], 0)?;
    if tenc.u8()? != 0 {
        return Err(Error::bitstream("invalid tenc reserved byte"));
    }
    let pattern = tenc.u8()?;
    if version == 0 && pattern != 0 {
        return Err(Error::bitstream("invalid tenc v0 pattern"));
    }
    let defaults = descriptor(&mut tenc, &scheme, pattern)?;
    tenc.end()?;
    Ok((frma, defaults))
}
#[derive(Clone, Default)]
pub(super) struct Groups {
    values: Vec<Defaults>,
    default: u32,
}
pub(super) fn groups(data: &[u8], default: Option<&Defaults>) -> Result<Groups> {
    let mut result = Groups::default();
    let mut found = false;
    for_each_box(data, |kind, payload| {
        if kind != b"sgpd" || payload.get(4..8) != Some(b"seig") {
            return Ok(());
        }
        if found {
            return Err(Error::bitstream("duplicate seig descriptions"));
        }
        found = true;
        let defaults = default.ok_or_else(|| Error::bitstream("seig without protected entry"))?;
        let mut r = Reader::new(payload);
        let (v, _) = r.full(&[0, 1, 2], 0)?;
        r.take(4)?;
        let length = if v >= 1 { r.u32()? as usize } else { 0 };
        if v >= 2 {
            result.default = r.u32()?;
        }
        let count = r.u32()? as usize;
        if count > 65_536 {
            return Err(Error::unsupported("protection descriptor budget exceeded"));
        }
        if count > payload.len() / 20 {
            return Err(Error::bitstream("seig count exceeds box"));
        }
        for _ in 0..count {
            let length = if v >= 1 && length == 0 {
                r.u32()? as usize
            } else {
                length
            };
            let start = r.pos;
            if r.u8()? != 0 {
                return Err(Error::bitstream("invalid seig reserved byte"));
            }
            let pattern = r.u8()?;
            let d = descriptor(&mut r, &defaults.scheme, pattern)?;
            if v >= 1 && r.pos - start != length {
                return Err(Error::bitstream("invalid seig description length"));
            }
            result.values.push(d);
        }
        r.end()?;
        if result.default as usize > result.values.len() {
            return Err(Error::bitstream("invalid default group"));
        }
        Ok(())
    })?;
    Ok(result)
}
fn mapped_defaults(
    traf: &[u8],
    default: &Defaults,
    track: &Groups,
    count: usize,
) -> Result<Vec<Defaults>> {
    let local = groups(traf, Some(default))?;
    let initial = if local.default != 0 {
        &local.values[local.default as usize - 1]
    } else if track.default != 0 {
        &track.values[track.default as usize - 1]
    } else {
        default
    };
    let mut values = vec![initial.clone(); count];
    let mut found = false;
    for_each_box(traf, |kind, payload| {
        if kind != b"sbgp" || payload.get(4..8) != Some(b"seig") {
            return Ok(());
        }
        if found {
            return Err(Error::bitstream("duplicate seig mapping"));
        }
        found = true;
        let mut r = Reader::new(payload);
        let (v, _) = r.full(&[0, 1], 0)?;
        r.take(4)?;
        if v == 1 && r.u32()? != 0 {
            return Err(Error::unsupported("seig grouping parameter"));
        }
        let n = r.u32()? as usize;
        if n > payload.len() / 8 {
            return Err(Error::bitstream("group count exceeds box"));
        }
        let mut pos = 0usize;
        for _ in 0..n {
            let run = r.u32()? as usize;
            let index = r.u32()?;
            let end = pos
                .checked_add(run)
                .filter(|e| *e <= count)
                .ok_or_else(|| Error::bitstream("sample group overflow"))?;
            if run == 0 {
                return Err(Error::bitstream("empty sample group run"));
            }
            let d = if index == 0 {
                initial
            } else if index > 0x10000 {
                local
                    .values
                    .get((index - 0x10001) as usize)
                    .ok_or_else(|| Error::bitstream("missing fragment group"))?
            } else {
                track
                    .values
                    .get((index - 1) as usize)
                    .ok_or_else(|| Error::bitstream("missing track group"))?
            };
            values[pos..end].fill(d.clone());
            pos = end;
        }
        if pos != count {
            return Err(Error::bitstream("sample group coverage mismatch"));
        }
        r.end()
    })?;
    Ok(values)
}
fn sample(r: &mut Reader<'_>, d: &Defaults, subs: bool, size: usize) -> Result<Option<Protection>> {
    if !d.encrypted {
        if subs && r.u16()? != 0 {
            return Err(Error::bitstream("clear sample has subsamples"));
        }
        return Ok(None);
    }
    let iv = if let Some(iv) = d.constant {
        Iv::Constant(iv)
    } else {
        Iv::Sample(r.take(d.iv_size)?.to_vec())
    };
    let mut subsamples = Vec::new();
    if subs {
        let count = usize::from(r.u16()?);
        if count > (r.data.len() - r.pos) / 6 {
            return Err(Error::bitstream("subsample count exceeds box"));
        }
        for _ in 0..count {
            subsamples.push(Subsample {
                clear: r.u16()?,
                encrypted: r.u32()?,
            });
        }
    }
    let p = Protection {
        scheme: d.scheme.clone(),
        kid: d.kid,
        iv,
        subsamples,
    };
    p.validate(size)?;
    Ok(Some(p))
}
pub(super) fn samples(
    traf: &[u8],
    segment: &[u8],
    base: u64,
    track: &InitTrack,
    sizes: &[usize],
    run_counts: &[usize],
) -> Result<Vec<Option<Protection>>> {
    let Some(default) = &track.protection else {
        if [b"senc", b"saiz", b"saio"]
            .iter()
            .any(|k| find_box(traf, k).ok().flatten().is_some())
        {
            return Err(Error::bitstream(
                "auxiliary encryption without protected entry",
            ));
        }
        groups(traf, None)?;
        if let Some(mapping) = unique(traf, b"sbgp")?
            && mapping.get(4..8) == Some(b"seig")
        {
            return Err(Error::bitstream(
                "sample encryption group without protected entry",
            ));
        }
        return Ok(vec![None; sizes.len()]);
    };
    let defaults = mapped_defaults(traf, default, &track.groups, sizes.len())?;
    let senc = unique(traf, b"senc")?;
    let mut inline = None;
    if let Some(data) = senc {
        let mut r = Reader::new(data);
        let (_, flags) = r.full(&[0], 2)?;
        if r.u32()? as usize != sizes.len() {
            return Err(Error::bitstream("senc sample count mismatch"));
        }
        let mut values = Vec::with_capacity(sizes.len());
        for (d, size) in defaults.iter().zip(sizes) {
            values.push(sample(&mut r, d, flags & 2 != 0, *size)?);
        }
        r.end()?;
        inline = Some(values);
    }
    let saiz = unique(traf, b"saiz")?;
    let saio = unique(traf, b"saio")?;
    let mut external = None;
    if let (Some(z), Some(o)) = (saiz, saio) {
        let mut z = Reader::new(z);
        let (_, zf) = z.full(&[0], 1)?;
        let ztype = if zf == 1 { Some(z.take(8)?) } else { None };
        let width = usize::from(z.u8()?);
        if z.u32()? as usize != sizes.len() {
            return Err(Error::bitstream("saiz sample count mismatch"));
        }
        let lengths = if width == 0 {
            z.take(sizes.len())?
                .iter()
                .map(|b| usize::from(*b))
                .collect::<Vec<_>>()
        } else {
            vec![width; sizes.len()]
        };
        z.end()?;
        let mut o = Reader::new(o);
        let (v, of) = o.full(&[0, 1], 1)?;
        let otype = if of == 1 { Some(o.take(8)?) } else { None };
        if ztype != otype
            || ztype.is_some_and(|t| {
                &t[..4]
                    != if matches!(default.scheme, Scheme::Cenc) {
                        b"cenc"
                    } else {
                        b"cbcs"
                    }
                    || t[4..] != [0; 4]
            })
        {
            return Err(Error::bitstream("auxiliary information type mismatch"));
        }
        let count = o.u32()? as usize;
        if count != 1 && count != run_counts.len() {
            return Err(Error::unsupported("saio must address resource or each run"));
        }
        let mut offsets = Vec::new();
        for _ in 0..count {
            let delta = if v == 0 {
                u64::from(o.u32()?)
            } else {
                o.u64()?
            };
            let offset = base
                .checked_add(delta)
                .and_then(|x| usize::try_from(x).ok())
                .filter(|x| *x <= segment.len())
                .ok_or_else(|| Error::bitstream("auxiliary offset overflow"))?;
            offsets.push(offset);
        }
        o.end()?;
        let mut values = Vec::with_capacity(sizes.len());
        let mut index = 0;
        let counts = if count == 1 {
            vec![sizes.len()]
        } else {
            run_counts.to_vec()
        };
        for (offset, n) in offsets.into_iter().zip(counts) {
            let mut data = Reader::new(&segment[offset..]);
            for _ in 0..n {
                let bytes = data.take(lengths[index])?;
                let mut r = Reader::new(bytes);
                let d = &defaults[index];
                let subs = bytes.len() > if d.encrypted { d.iv_size } else { 0 };
                values.push(sample(&mut r, d, subs, sizes[index])?);
                r.end()?;
                index += 1;
            }
        }
        external = Some(values);
    } else if saiz.is_some() || saio.is_some() {
        return Err(Error::bitstream("incomplete auxiliary location"));
    }
    match (inline, external) {
        (Some(a), Some(b)) if a != b => Err(Error::bitstream(
            "conflicting encryption auxiliary metadata",
        )),
        (Some(a), _) | (_, Some(a)) => Ok(a),
        _ => defaults
            .iter()
            .zip(sizes)
            .map(|(d, size)| sample(&mut Reader::new(&[]), d, false, *size))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut b = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        b.extend_from_slice(kind);
        b.extend_from_slice(payload);
        b
    }
    fn track() -> InitTrack {
        parse_init_segment(include_bytes!(
            "../../tests/fixtures/sample_crypto/fmp4_avc_cenc/init.mp4"
        ))
        .unwrap()
        .remove(0)
    }
    fn senc(iv: &[u8]) -> Vec<u8> {
        let mut b = vec![0; 4];
        b.extend_from_slice(&1u32.to_be_bytes());
        b.extend_from_slice(iv);
        boxed(b"senc", &b)
    }
    fn aux(offset: u64, v: u8, size: u8) -> Vec<u8> {
        let mut z = vec![0; 4];
        z.push(size);
        z.extend_from_slice(&1u32.to_be_bytes());
        let mut o = vec![v, 0, 0, 0];
        o.extend_from_slice(&1u32.to_be_bytes());
        if v == 0 {
            o.extend_from_slice(&(offset as u32).to_be_bytes())
        } else {
            o.extend_from_slice(&offset.to_be_bytes())
        }
        [boxed(b"saiz", &z), boxed(b"saio", &o)].concat()
    }
    #[test]
    fn inline_auxiliary_agreement_offsets_versions_and_subsamples() {
        let mut track = track();
        track.protection.as_mut().unwrap().iv_size = 16;
        for v in [0, 1] {
            let data = [vec![0; 19], vec![7; 16]].concat();
            let traf = aux(12, v, 16);
            let parsed = samples(&traf, &data, 7, &track, &[32], &[1]).unwrap();
            assert_eq!(parsed[0].as_ref().unwrap().iv, Iv::Sample(vec![7; 16]));
            let both = [senc(&[7; 16]), traf.clone()].concat();
            assert_eq!(
                samples(&both, &data, 7, &track, &[32], &[1]).unwrap(),
                parsed
            );
            let conflict = [senc(&[8; 16]), traf.clone()].concat();
            assert!(samples(&conflict, &data, 7, &track, &[32], &[1]).is_err());
            assert!(samples(&traf, &data, 8, &track, &[32], &[1]).is_err());
        }
        let mut inline = vec![0, 0, 0, 2, 0, 0, 0, 1];
        inline.extend_from_slice(&[7; 16]);
        inline.extend_from_slice(&[0, 2, 0, 2, 0, 0, 0, 8, 0, 3, 0, 0, 0, 19]);
        assert!(samples(&boxed(b"senc", &inline), &[], 0, &track, &[32], &[1]).is_ok());
        for length in [31, 33] {
            assert!(samples(&boxed(b"senc", &inline), &[], 0, &track, &[length], &[1]).is_err());
        }
        for version in [1, 2, 255] {
            let mut b = inline.clone();
            b[0] = version;
            assert!(samples(&boxed(b"senc", &b), &[], 0, &track, &[32], &[1]).is_err());
        }
        for n in 0..inline.len() {
            assert!(samples(&boxed(b"senc", &inline[..n]), &[], 0, &track, &[32], &[1]).is_err());
        }
        assert!(
            samples(
                &[senc(&[7; 16]), senc(&[7; 16])].concat(),
                &[],
                0,
                &track,
                &[32],
                &[1]
            )
            .is_err()
        );
    }
    fn seig(encrypted: bool, kid: u8) -> Vec<u8> {
        let mut d = vec![0, 0, u8::from(encrypted), if encrypted { 16 } else { 0 }];
        d.extend_from_slice(&[kid; 16]);
        d
    }
    fn sgpd(v: u8, d: &[u8]) -> Vec<u8> {
        let mut b = vec![v, 0, 0, 0];
        b.extend_from_slice(b"seig");
        if v >= 1 {
            b.extend_from_slice(&(d.len() as u32).to_be_bytes());
        }
        if v == 2 {
            b.extend_from_slice(&1u32.to_be_bytes());
        }
        b.extend_from_slice(&1u32.to_be_bytes());
        b.extend_from_slice(d);
        boxed(b"sgpd", &b)
    }
    fn sbgp(v: u8, index: u32, count: u32) -> Vec<u8> {
        let mut b = vec![v, 0, 0, 0];
        b.extend_from_slice(b"seig");
        if v == 1 {
            b.extend_from_slice(&0u32.to_be_bytes());
        }
        b.extend_from_slice(&1u32.to_be_bytes());
        b.extend_from_slice(&count.to_be_bytes());
        b.extend_from_slice(&index.to_be_bytes());
        boxed(b"sbgp", &b)
    }
    #[test]
    fn track_fragment_groups_clear_overrides_kids_and_default_indices() {
        for v in [0, 1, 2] {
            for mapping_version in [0, 1] {
                let mut t = track();
                t.protection.as_mut().unwrap().iv_size = 16;
                t.groups = groups(&sgpd(v, &seig(true, 2)), t.protection.as_ref()).unwrap();
                let traf = [sbgp(mapping_version, 1, 1), senc(&[7; 16])].concat();
                assert_eq!(
                    samples(&traf, &[], 0, &t, &[32], &[1]).unwrap()[0]
                        .as_ref()
                        .unwrap()
                        .kid,
                    [2; 16]
                );
                let traf = [
                    sgpd(v, &seig(true, 3)),
                    sbgp(mapping_version, 0x10001, 1),
                    senc(&[7; 16]),
                ]
                .concat();
                assert_eq!(
                    samples(&traf, &[], 0, &t, &[32], &[1]).unwrap()[0]
                        .as_ref()
                        .unwrap()
                        .kid,
                    [3; 16]
                );
                let traf = [sgpd(v, &seig(false, 3)), sbgp(mapping_version, 0x10001, 1)].concat();
                assert_eq!(samples(&traf, &[], 0, &t, &[32], &[1]).unwrap(), vec![None]);
                for (index, count) in [(2, 1), (0x10000, 1), (0x10002, 1), (1, 0), (1, 2)] {
                    let traf = [sbgp(mapping_version, index, count), senc(&[7; 16])].concat();
                    assert!(samples(&traf, &[], 0, &t, &[32], &[1]).is_err());
                }
            }
        }
        let mut t = track();
        let defaults = t.protection.as_ref().unwrap().clone();
        t.groups = groups(&sgpd(2, &seig(true, 4)), Some(&defaults)).unwrap();
        assert_eq!(
            samples(&senc(&[8; 16]), &[], 0, &t, &[32], &[1]).unwrap()[0]
                .as_ref()
                .unwrap()
                .kid,
            [4; 16]
        );
    }
    #[test]
    fn constant_iv_clear_mixing_and_unprotected_metadata_rejection() {
        let mut t = track();
        let p = t.protection.as_mut().unwrap();
        p.scheme = Scheme::Cbcs { crypt: 1, skip: 9 };
        p.iv_size = 0;
        p.constant = Some([4; 16]);
        assert_eq!(
            samples(&[], &[], 0, &t, &[17, 48], &[2]).unwrap()[1]
                .as_ref()
                .unwrap()
                .iv,
            Iv::Constant([4; 16])
        );
        t.protection = None;
        assert!(samples(&senc(&[0; 16]), &[], 0, &t, &[32], &[1]).is_err());
        assert!(samples(&sgpd(1, &seig(true, 1)), &[], 0, &t, &[32], &[1]).is_err());
    }
}

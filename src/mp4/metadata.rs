//! Track metadata edits preserve all codec/sample-table bytes.
use super::*;

pub(super) fn decorate(trak: Vec<u8>, metadata: Option<&crate::TrackMetadata>) -> Result<Vec<u8>> {
    let Some(metadata) = metadata else {
        return Ok(trak);
    };
    rewrite(&trak, metadata)
}
fn rewrite(bytes: &[u8], meta: &crate::TrackMetadata) -> Result<Vec<u8>> {
    let mut result = Vec::new();
    let mut pos = 0;
    while pos < bytes.len() {
        if bytes.len() - pos < 8 {
            return Err(Error::muxing("truncated metadata box"));
        }
        let size = u32::from_be_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
        let end = pos
            .checked_add(size)
            .filter(|e| size >= 8 && *e <= bytes.len())
            .ok_or_else(|| Error::muxing("invalid metadata box"))?;
        let kind: &[u8; 4] = bytes[pos + 4..pos + 8].try_into().unwrap();
        let mut payload = bytes[pos + 8..end].to_vec();
        match kind {
            b"trak" | b"mdia" => {
                payload = rewrite(&payload, meta)?;
                if kind == b"mdia" {
                    payload.extend_from_slice(&full_box(b"elng", 0, 0, |out| {
                        out.extend_from_slice(meta.language.as_bytes());
                        out.push(0);
                    }));
                }
            }
            b"tkhd" => {
                if payload.len() < 40 {
                    return Err(Error::muxing("short tkhd"));
                }
                payload[3] = if meta.default { 7 } else { 6 };
                let group = if payload[0] == 1 { 46 } else { 34 };
                if payload.len() < group + 2 {
                    return Err(Error::muxing("short tkhd"));
                }
                payload[group..group + 2].copy_from_slice(&meta.group.to_be_bytes());
            }
            b"mdhd" => {
                let n = payload.len();
                if n < 4 {
                    return Err(Error::muxing("short mdhd"));
                }
                let lang = match meta.language.as_str() {
                    "en" => "eng",
                    "zh" => "zho",
                    "ja" => "jpn",
                    "fr" => "fra",
                    "de" => "deu",
                    "es" => "spa",
                    v if v.len() == 3 && v.bytes().all(|b| b.is_ascii_lowercase()) => v,
                    _ => "und",
                };
                let language = lang
                    .bytes()
                    .fold(0u16, |n, b| (n << 5) | u16::from(b - 0x60));
                payload[n - 4..n - 2].copy_from_slice(&language.to_be_bytes());
            }
            b"hdlr" => {
                if payload.len() < 24 {
                    return Err(Error::muxing("short hdlr"));
                }
                payload.truncate(24);
                payload.extend_from_slice(meta.name.as_bytes());
                payload.push(0);
            }
            b"elng" => {
                pos = end;
                continue;
            }
            _ => {}
        }
        result.extend_from_slice(&boxed(kind, |out| out.extend_from_slice(&payload)));
        pos = end;
    }
    Ok(result)
}

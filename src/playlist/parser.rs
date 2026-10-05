use super::*;
use PlaylistErrorKind as E;
use std::collections::BTreeSet;
fn err(kind: E) -> PlaylistError {
    PlaylistError::new(kind)
}
fn integer(text: &str) -> PlaylistResult<u64> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(err(E::InvalidInteger));
    }
    text.parse().map_err(|_| err(E::Overflow))
}
fn duration(text: &str) -> PlaylistResult<PlaylistDuration> {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    if text.ends_with('.') || whole.is_empty() || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return Err(err(E::InvalidDuration));
    }
    let fraction = fraction.trim_end_matches('0');
    if fraction.len() > 9 {
        return Err(err(E::InvalidDuration));
    }
    let scale = 10u32.pow(fraction.len() as u32);
    let ticks = integer(whole)?
        .checked_mul(u64::from(scale))
        .and_then(|x| {
            x.checked_add(if fraction.is_empty() {
                0
            } else {
                fraction.parse().ok()?
            })
        })
        .ok_or_else(|| err(E::Overflow))?;
    Ok(PlaylistDuration {
        ticks,
        timescale: scale,
    })
}
fn date_time(text: &str) -> PlaylistResult<String> {
    let bad = || err(E::InvalidDateTime);
    let bytes = text.as_bytes();
    if !text.is_ascii()
        || bytes.len() < 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !matches!(bytes[10], b'T' | b't')
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return Err(bad());
    }
    let number = |s: &str| integer(s).map_err(|_| bad());
    let year = number(&text[..4])?;
    let month = number(&text[5..7])?;
    let day = number(&text[8..10])?;
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        _ => return Err(bad()),
    };
    if day == 0
        || day > days
        || number(&text[11..13])? > 23
        || number(&text[14..16])? > 59
        || number(&text[17..19])? > 60
    {
        return Err(bad());
    }
    let mut tail = &text[19..];
    if let Some(fraction) = tail.strip_prefix('.') {
        let n = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if n == 0 {
            return Err(bad());
        }
        tail = &fraction[n..];
    }
    if tail != "Z"
        && tail != "z"
        && (tail.len() != 6
            || !matches!(tail.as_bytes()[0], b'+' | b'-')
            || tail.as_bytes()[3] != b':'
            || number(&tail[1..3])? > 23
            || number(&tail[4..6])? > 59)
    {
        return Err(bad());
    }
    Ok(text.to_owned())
}
#[derive(Clone)]
struct Attribute {
    text: String,
    quoted: bool,
}
fn attributes(text: &str) -> PlaylistResult<BTreeMap<String, Attribute>> {
    let mut out = BTreeMap::new();
    let mut rest = text;
    while !rest.is_empty() {
        let (name, value) = rest
            .split_once('=')
            .ok_or_else(|| err(E::InvalidAttribute))?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(err(E::InvalidAttribute));
        }
        let (value, quoted, tail) = if let Some(v) = value.strip_prefix('"') {
            let end = v.find('"').ok_or_else(|| err(E::InvalidAttribute))?;
            (&v[..end], true, &v[end + 1..])
        } else {
            let end = value.find(',').unwrap_or(value.len());
            let item = &value[..end];
            if item.is_empty() || item.bytes().any(|b| b.is_ascii_whitespace() || b == b'"') {
                return Err(err(E::InvalidAttribute));
            }
            (item, false, &value[end..])
        };
        if out
            .insert(
                name.to_owned(),
                Attribute {
                    text: value.to_owned(),
                    quoted,
                },
            )
            .is_some()
        {
            return Err(err(E::InvalidAttribute));
        }
        rest = if tail.is_empty() {
            ""
        } else {
            tail.strip_prefix(',')
                .filter(|s| !s.is_empty())
                .ok_or_else(|| err(E::InvalidAttribute))?
        };
    }
    if out.is_empty() {
        return Err(err(E::InvalidAttribute));
    }
    Ok(out)
}
fn attr<'a>(
    attrs: &'a BTreeMap<String, Attribute>,
    name: &str,
    quoted: bool,
) -> PlaylistResult<Option<&'a str>> {
    attrs
        .get(name)
        .map(|v| {
            if v.quoted != quoted {
                Err(err(E::InvalidAttribute))
            } else {
                Ok(v.text.as_str())
            }
        })
        .transpose()
}
fn required<'a>(
    attrs: &'a BTreeMap<String, Attribute>,
    name: &str,
    quoted: bool,
) -> PlaylistResult<&'a str> {
    attr(attrs, name, quoted)?
        .filter(|s| !s.is_empty())
        .ok_or_else(|| err(E::InvalidAttribute))
}
fn key(
    text: &str,
    base: &ResourceLocation,
    id: DeclarationId,
    session: bool,
) -> PlaylistResult<Option<KeyReference>> {
    let attrs = attributes(text)?;
    let method = required(&attrs, "METHOD", false)?;
    if method == "NONE" {
        if attrs.len() != 1 || session {
            return Err(err(E::InvalidKey));
        }
        return Ok(None);
    }
    let method = match method {
        "AES-128" => EncryptionMethod::Aes128,
        "SAMPLE-AES" => EncryptionMethod::SampleAes,
        "SAMPLE-AES-CTR" => EncryptionMethod::SampleAesCtr,
        "AES-256-GCM" => EncryptionMethod::Aes256Gcm,
        other => EncryptionMethod::Other(other.to_owned()),
    };
    let location = base.resolve(required(&attrs, "URI", true)?)?;
    let format = attr(&attrs, "KEYFORMAT", true)?.unwrap_or("identity");
    if format.is_empty() {
        return Err(err(E::InvalidKey));
    }
    let mut versions = attr(&attrs, "KEYFORMATVERSIONS", true)?
        .unwrap_or("1")
        .split('/')
        .map(|part| {
            u32::try_from(integer(part)?)
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| err(E::InvalidKey))
        })
        .collect::<PlaylistResult<Vec<_>>>()?;
    versions.sort_unstable();
    versions.dedup();
    let iv = attr(&attrs, "IV", false)?
        .map(|s| {
            let digits = s
                .strip_prefix("0x")
                .or_else(|| s.strip_prefix("0X"))
                .ok_or_else(|| err(E::InvalidIv))?;
            if digits.is_empty()
                || digits.len() > 32
                || !digits.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err(err(E::InvalidIv));
            }
            u128::from_str_radix(digits, 16)
                .map(u128::to_be_bytes)
                .map_err(|_| err(E::InvalidIv))
        })
        .transpose()?;
    let extensions = attrs
        .iter()
        .filter(|(k, _)| {
            !matches!(
                k.as_str(),
                "METHOD" | "URI" | "KEYFORMAT" | "KEYFORMATVERSIONS" | "IV"
            )
        })
        .map(|(k, v)| (k.clone(), v.text.clone()))
        .collect();
    Ok(Some(KeyReference {
        declaration: id,
        method,
        location,
        format: format.to_owned(),
        versions,
        iv,
        extensions,
    }))
}
fn range(
    text: &str,
    location: &ResourceLocation,
    previous: Option<&(ResourceLocation, ResourceRange)>,
) -> PlaylistResult<ResourceRange> {
    let (n, o) = text
        .split_once('@')
        .map_or((text, None), |(n, o)| (n, Some(o)));
    let length = integer(n)?;
    let offset = if let Some(o) = o {
        integer(o)?
    } else {
        let (_, prev) = previous
            .filter(|(loc, _)| loc == location)
            .ok_or_else(|| err(E::InvalidRange))?;
        prev.offset
            .checked_add(prev.length)
            .ok_or_else(|| err(E::Overflow))?
    };
    if length == 0 {
        return Err(err(E::InvalidRange));
    }
    offset.checked_add(length).ok_or_else(|| err(E::Overflow))?;
    Ok(ResourceRange { offset, length })
}
fn lines(text: &str) -> PlaylistResult<Vec<(usize, &str)>> {
    let lines = text
        .lines()
        .enumerate()
        .map(|(i, l)| (i + 1, l.trim()))
        .filter(|(_, l)| !l.is_empty())
        .collect::<Vec<_>>();
    if lines.first().map(|(_, l)| *l) != Some("#EXTM3U") {
        return Err(err(E::Syntax).at(1));
    }
    Ok(lines)
}
fn is_master(tag: &str) -> bool {
    matches!(
        tag,
        "#EXT-X-STREAM-INF"
            | "#EXT-X-MEDIA"
            | "#EXT-X-I-FRAME-STREAM-INF"
            | "#EXT-X-SESSION-KEY"
            | "#EXT-X-SESSION-DATA"
    )
}
fn is_media(tag: &str) -> bool {
    matches!(
        tag,
        "#EXTINF"
            | "#EXT-X-KEY"
            | "#EXT-X-MAP"
            | "#EXT-X-MEDIA-SEQUENCE"
            | "#EXT-X-DISCONTINUITY-SEQUENCE"
            | "#EXT-X-DISCONTINUITY"
            | "#EXT-X-ENDLIST"
            | "#EXT-X-PLAYLIST-TYPE"
            | "#EXT-X-TARGETDURATION"
            | "#EXT-X-BYTERANGE"
            | "#EXT-X-GAP"
            | "#EXT-X-PROGRAM-DATE-TIME"
            | "#EXT-X-I-FRAMES-ONLY"
    )
}
/// Parse a selected media playlist without I/O. Uses the final redirect location as URI base.
/// Open/empty/EVENT snapshots and unsupported methods are retained, not executed.
pub fn parse_playlist_snapshot(
    resource: &TextResource,
    context: PlaylistContext,
) -> PlaylistResult<PlaylistSnapshot> {
    let lines = lines(&resource.content)?;
    let mut out = SnapshotData {
        schema_version: 1,
        version: None,
        context,
        location: ResourceLocation(resource.location.clone()),
        source: resource.content.clone(),
        playlist_type: None,
        end_list: false,
        target_duration: None,
        media_sequence: 0,
        discontinuity_sequence: 0,
        independent_segments: false,
        iframe_only: false,
        segments: vec![],
        retained_tags: vec![],
    };
    let mut keys = KeyContext::default();
    let mut map = None;
    let mut declaration = 0u64;
    let mut epoch = 0u64;
    let mut disc = false;
    let mut pending_duration = None;
    let mut pending_range: Option<String> = None;
    let mut pdt = None;
    let mut gap = false;
    let mut previous_range = None;
    let mut seen = BTreeSet::new();
    let mut saw_media = false;
    for (number, line) in lines.into_iter().skip(1) {
        let result = (|| -> PlaylistResult<()> {
            if !line.starts_with('#') {
                let location = out.location.resolve(line)?;
                let sequence = out
                    .media_sequence
                    .checked_add(u64::try_from(out.segments.len()).map_err(|_| err(E::Overflow))?)
                    .ok_or_else(|| err(E::Overflow))?;
                let duration = pending_duration.take().ok_or_else(|| err(E::Syntax))?;
                let range = pending_range
                    .take()
                    .map(|v| range(&v, &location, previous_range.as_ref()))
                    .transpose()?;
                previous_range = range.map(|r| (location.clone(), r));
                out.segments.push(SegmentDescriptor {
                    slot: SegmentSlot {
                        input_id: out.context.input_id.clone(),
                        generation: out.context.generation,
                        sequence,
                        epoch,
                    },
                    location,
                    range,
                    duration,
                    program_date_time: pdt.take(),
                    gap,
                    discontinuity: disc,
                    map: map.clone(),
                    keys: keys.clone(),
                });
                gap = false;
                disc = false;
                saw_media = true;
                return Ok(());
            }
            if !line.starts_with("#EXT") {
                return Ok(());
            }
            let (tag, value) = line
                .split_once(':')
                .map_or((line, None), |(t, v)| (t, Some(v)));
            if is_master(tag) {
                return Err(err(if saw_media {
                    E::MixedPlaylist
                } else {
                    E::MasterPlaylist
                }));
            }
            let required_value = || value.ok_or_else(|| err(E::Syntax));
            if matches!(
                tag,
                "#EXT-X-TARGETDURATION"
                    | "#EXT-X-MEDIA-SEQUENCE"
                    | "#EXT-X-DISCONTINUITY-SEQUENCE"
                    | "#EXT-X-PLAYLIST-TYPE"
                    | "#EXT-X-ENDLIST"
                    | "#EXT-X-VERSION"
                    | "#EXT-X-INDEPENDENT-SEGMENTS"
                    | "#EXT-X-I-FRAMES-ONLY"
            ) && !seen.insert(tag.to_owned())
            {
                return Err(err(E::DuplicateTag));
            }
            if matches!(
                tag,
                "#EXT-X-ENDLIST"
                    | "#EXT-X-DISCONTINUITY"
                    | "#EXT-X-GAP"
                    | "#EXT-X-INDEPENDENT-SEGMENTS"
                    | "#EXT-X-I-FRAMES-ONLY"
            ) && value.is_some()
            {
                return Err(err(E::Syntax));
            }
            match tag {
                "#EXTM3U" => return Err(err(E::DuplicateTag)),
                "#EXTINF" => {
                    if pending_duration.is_some() {
                        return Err(err(E::Syntax));
                    }
                    let (v, _) = required_value()?
                        .split_once(',')
                        .ok_or_else(|| err(E::Syntax))?;
                    pending_duration = Some(duration(v)?);
                }
                "#EXT-X-TARGETDURATION" => {
                    let v = integer(required_value()?)?;
                    if v == 0 {
                        return Err(err(E::InvalidDuration));
                    }
                    out.target_duration = Some(v)
                }
                "#EXT-X-MEDIA-SEQUENCE" => {
                    if !out.segments.is_empty() {
                        return Err(err(E::Syntax));
                    }
                    out.media_sequence = integer(required_value()?)?;
                }
                "#EXT-X-DISCONTINUITY-SEQUENCE" => {
                    if !out.segments.is_empty() || disc {
                        return Err(err(E::Syntax));
                    }
                    epoch = integer(required_value()?)?;
                    out.discontinuity_sequence = epoch
                }
                "#EXT-X-DISCONTINUITY" => {
                    epoch = epoch.checked_add(1).ok_or_else(|| err(E::Overflow))?;
                    disc = true;
                }
                "#EXT-X-PLAYLIST-TYPE" => {
                    out.playlist_type = Some(match required_value()? {
                        "VOD" => PlaylistType::Vod,
                        "EVENT" => PlaylistType::Event,
                        _ => return Err(err(E::Syntax)),
                    });
                }
                "#EXT-X-ENDLIST" => out.end_list = true,
                "#EXT-X-INDEPENDENT-SEGMENTS" => out.independent_segments = true,
                "#EXT-X-I-FRAMES-ONLY" => out.iframe_only = true,
                "#EXT-X-BYTERANGE" => {
                    if pending_range.is_some() {
                        return Err(err(E::Syntax));
                    }
                    pending_range = Some(required_value()?.to_owned())
                }
                "#EXT-X-PROGRAM-DATE-TIME" => {
                    if pdt.is_some() {
                        return Err(err(E::DuplicateTag));
                    }
                    pdt = Some(date_time(required_value()?)?)
                }
                "#EXT-X-GAP" => {
                    if gap {
                        return Err(err(E::DuplicateTag));
                    }
                    gap = true
                }
                "#EXT-X-VERSION" => {
                    let version = integer(required_value()?)?;
                    if version == 0 {
                        return Err(err(E::InvalidInteger));
                    }
                    out.version = Some(version);
                }
                "#EXT-X-KEY" => {
                    declaration = declaration.checked_add(1).ok_or_else(|| err(E::Overflow))?;
                    match key(
                        required_value()?,
                        &out.location,
                        DeclarationId {
                            revision: out.context.revision,
                            ordinal: declaration,
                        },
                        false,
                    )? {
                        None => keys = KeyContext::default(),
                        Some(key) => {
                            keys.candidates.retain(|k| k.format != key.format);
                            keys.candidates.push(key)
                        }
                    }
                }
                "#EXT-X-MAP" => {
                    declaration = declaration.checked_add(1).ok_or_else(|| err(E::Overflow))?;
                    let attrs = attributes(required_value()?)?;
                    let location = out.location.resolve(required(&attrs, "URI", true)?)?;
                    // RFC 8216's implicit offset references the preceding media segment, not a MAP-global cursor.
                    let range = attr(&attrs, "BYTERANGE", true)?
                        .map(|v| range(v, &location, previous_range.as_ref()))
                        .transpose()?;
                    if attrs
                        .keys()
                        .any(|k| !matches!(k.as_str(), "URI" | "BYTERANGE"))
                    {
                        out.retained_tags.push(RetainedTag {
                            text: line.to_owned(),
                        })
                    }
                    map = Some(MapDescriptor {
                        declaration: DeclarationId {
                            revision: out.context.revision,
                            ordinal: declaration,
                        },
                        location,
                        range,
                        keys: keys.clone(),
                    });
                }
                _ => out.retained_tags.push(RetainedTag {
                    text: line.to_owned(),
                }),
            }
            saw_media |= is_media(tag);
            Ok(())
        })();
        result.map_err(|e| e.at(number))?;
    }
    if pending_duration.is_some() || pending_range.is_some() || pdt.is_some() || gap || disc {
        return Err(err(E::Syntax));
    }
    Ok(PlaylistSnapshot(out))
}
/// Extract and validate master SESSION-KEY hints only. Does not select renditions or fetch keys.
/// Rejects media tags; pass the media playlist separately to `parse_playlist_snapshot`.
pub fn parse_session_keys(
    resource: &TextResource,
    context: &PlaylistContext,
) -> PlaylistResult<Vec<KeyReference>> {
    let base = ResourceLocation(resource.location.clone());
    let mut hints: Vec<KeyReference> = vec![];
    for (number, line) in lines(&resource.content)?.into_iter().skip(1) {
        let (tag, value) = line
            .split_once(':')
            .map_or((line, None), |(a, b)| (a, Some(b)));
        if is_media(tag) {
            return Err(err(E::MixedPlaylist).at(number));
        }
        if tag == "#EXT-X-SESSION-KEY" {
            let result = (|| {
                let mut k = key(
                    value.ok_or_else(|| err(E::Syntax))?,
                    &base,
                    DeclarationId {
                        revision: context.revision,
                        ordinal: hints.len() as u64,
                    },
                    true,
                )?
                .ok_or_else(|| err(E::InvalidKey))?;
                if hints.iter().any(|h| {
                    h.method == k.method
                        && h.location == k.location
                        && h.format == k.format
                        && h.versions == k.versions
                        && h.iv == k.iv
                }) {
                    return Err(err(E::InvalidKey));
                }
                // Same local namespace as other declarations: ordinals begin at one.
                k.declaration.ordinal = k
                    .declaration
                    .ordinal
                    .checked_add(1)
                    .ok_or_else(|| err(E::Overflow))?;
                hints.push(k);
                Ok(())
            })();
            result.map_err(|e: PlaylistError| e.at(number))?;
        }
    }
    Ok(hints)
}

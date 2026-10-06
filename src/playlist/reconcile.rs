//! Reconciliation helpers kept beside the immutable parser model.
use super::*;

fn keys_equivalent(a: &KeyContext, b: &KeyContext) -> bool {
    a.candidates.len() == b.candidates.len()
        && a.candidates.iter().zip(&b.candidates).all(|(a, b)| {
            a.method == b.method
                && a.location == b.location
                && a.format == b.format
                && a.versions == b.versions
                && a.iv == b.iv
                && a.extensions == b.extensions
        })
}
pub(crate) fn equivalent(a: &SegmentDescriptor, b: &SegmentDescriptor) -> bool {
    a.slot == b.slot
        && a.location == b.location
        && a.range == b.range
        && a.duration == b.duration
        && a.program_date_time == b.program_date_time
        && a.gap == b.gap
        && keys_equivalent(&a.keys, &b.keys)
        && match (&a.map, &b.map) {
            (None, None) => true,
            (Some(a), Some(b)) => {
                a.location == b.location && a.range == b.range && keys_equivalent(&a.keys, &b.keys)
            }
            _ => false,
        }
}
#[derive(Default)]
pub(crate) struct Declarations {
    keys: Vec<(DeclarationId, DeclarationId)>,
    maps: Vec<(DeclarationId, DeclarationId)>,
}
fn bind(
    bindings: &mut Vec<(DeclarationId, DeclarationId)>,
    a: DeclarationId,
    b: DeclarationId,
) -> bool {
    if let Some((_, old)) = bindings.iter().find(|(id, _)| *id == a) {
        return *old == b;
    }
    bindings.push((a, b));
    true
}
impl Declarations {
    pub(crate) fn observe(&mut self, current: &SegmentDescriptor, old: &SegmentDescriptor) -> bool {
        if !equivalent(current, old) {
            return false;
        }
        let mut pairs = vec![(&current.keys, &old.keys)];
        if let (Some(a), Some(b)) = (&current.map, &old.map) {
            if !bind(&mut self.maps, a.declaration, b.declaration) {
                return false;
            }
            pairs.push((&a.keys, &b.keys));
        }
        for (a, b) in pairs {
            for (a, b) in a.candidates.iter().zip(&b.candidates) {
                if !bind(&mut self.keys, a.declaration, b.declaration) {
                    return false;
                }
            }
        }
        true
    }
    pub(crate) fn apply(&self, segment: &mut SegmentDescriptor) {
        let apply_keys = |keys: &mut KeyContext| {
            for key in &mut keys.candidates {
                if let Some((_, id)) = self.keys.iter().find(|(id, _)| *id == key.declaration) {
                    key.declaration = *id;
                }
            }
        };
        apply_keys(&mut segment.keys);
        if let Some(map) = &mut segment.map {
            apply_keys(&mut map.keys);
            if let Some((_, id)) = self.maps.iter().find(|(id, _)| *id == map.declaration) {
                map.declaration = *id;
            }
        }
    }
}
/// Owned metadata accounting includes authentication queries (never logged).
pub(crate) fn metadata_bytes(s: &SegmentDescriptor) -> usize {
    fn location(l: &ResourceLocation) -> usize {
        match l.location() {
            SourceLocation::Url(u) => u.as_str().len(),
            SourceLocation::File(p) => p.as_os_str().len(),
        }
    }
    fn keys(k: &KeyContext) -> usize {
        k.candidates
            .iter()
            .map(|k| {
                std::mem::size_of::<KeyReference>()
                    + location(&k.location)
                    + k.format.len()
                    + k.versions.len() * 4
                    + k.extensions
                        .iter()
                        .map(|(a, b)| a.len() + b.len())
                        .sum::<usize>()
            })
            .sum()
    }
    std::mem::size_of::<SegmentDescriptor>()
        + s.slot.input_id.0.len()
        + location(&s.location)
        + s.program_date_time.as_ref().map_or(0, String::len)
        + keys(&s.keys)
        + s.map.as_ref().map_or(0, |m| {
            std::mem::size_of::<MapDescriptor>() + location(&m.location) + keys(&m.keys)
        })
}

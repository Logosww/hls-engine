//! Wire integers are strings. A snapshot is a validated archive, not a checkpoint or diagnostic.
use super::*;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};
pub(super) mod u64_string {
    use super::*;
    pub fn serialize<S: Serializer>(v: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        let s = String::deserialize(d)?;
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return Err(D::Error::custom("invalid decimal u64 string"));
        }
        s.parse().map_err(|_| D::Error::custom("u64 overflow"))
    }
}
pub(super) mod optional_u64_string {
    use super::*;
    pub fn serialize<S: Serializer>(v: &Option<u64>, s: S) -> Result<S::Ok, S::Error> {
        v.map(|x| x.to_string()).serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
        Option::<String>::deserialize(d)?
            .map(|s| {
                if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(D::Error::custom("invalid decimal u64 string"));
                }
                s.parse().map_err(|_| D::Error::custom("u64 overflow"))
            })
            .transpose()
    }
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", deny_unknown_fields)]
pub(super) enum Location {
    Url(String),
    File(std::path::PathBuf),
}
impl From<ResourceLocation> for Location {
    fn from(v: ResourceLocation) -> Self {
        match v.0 {
            SourceLocation::Url(u) => Self::Url(u.to_string()),
            SourceLocation::File(p) => Self::File(p),
        }
    }
}
impl TryFrom<Location> for ResourceLocation {
    type Error = PlaylistError;
    fn try_from(v: Location) -> PlaylistResult<Self> {
        Ok(Self(match v {
            Location::Url(u) => SourceLocation::Url(
                url::Url::parse(&u)
                    .map_err(|_| PlaylistError::new(PlaylistErrorKind::InvalidLocation))?,
            ),
            Location::File(p) => SourceLocation::File(p),
        }))
    }
}
impl Serialize for PlaylistSnapshot {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}
impl<'de> Deserialize<'de> for PlaylistSnapshot {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let data = SnapshotData::deserialize(d)?;
        let parsed = parse_playlist_snapshot(
            &TextResource {
                content: data.source.clone(),
                location: data.location.0.clone(),
            },
            data.context.clone(),
        )
        .map_err(D::Error::custom)?;
        if parsed.0 != data {
            return Err(D::Error::custom(
                "playlist projection does not match validated source",
            ));
        }
        Ok(parsed)
    }
}

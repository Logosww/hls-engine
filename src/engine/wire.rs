use super::*;
use serde::{
    Deserialize, Deserializer, Serialize, Serializer, de::Error as _, ser::SerializeStruct,
};

impl Serialize for MediaTime {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = s.serialize_struct("MediaTime", 2)?;
        map.serialize_field("ticks", &self.ticks.to_string())?;
        map.serialize_field("timescale", &self.timescale)?;
        map.end()
    }
}
impl<'de> Deserialize<'de> for MediaTime {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            ticks: String,
            timescale: u32,
        }
        let value = Wire::deserialize(d)?;
        let ticks = value.ticks.parse::<i128>().map_err(D::Error::custom)?;
        if ticks.to_string() != value.ticks {
            return Err(D::Error::custom("non-canonical signed ticks"));
        }
        MediaTime::new(ticks, value.timescale).map_err(D::Error::custom)
    }
}
impl Serialize for PresentationRange {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = s.serialize_struct("PresentationRange", 2)?;
        map.serialize_field("start", &self.start)?;
        map.serialize_field("end", &self.end)?;
        map.end()
    }
}
impl<'de> Deserialize<'de> for PresentationRange {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            start: MediaTime,
            end: MediaTime,
        }
        let value = Wire::deserialize(d)?;
        Self::new(value.start, value.end).map_err(D::Error::custom)
    }
}

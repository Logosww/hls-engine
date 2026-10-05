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
impl Serialize for EpochMapping {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = s.serialize_struct("EpochMapping", 11)?;
        map.serialize_field("input_id", &self.input)?;
        map.serialize_field("track_id", &self.track)?;
        map.serialize_field("epoch", &self.epoch.to_string())?;
        map.serialize_field("source_origin", &self.source_origin)?;
        map.serialize_field("source_decode_start", &self.source_decode_start)?;
        map.serialize_field("configuration_id", &self.config_id)?;
        map.serialize_field("presentation", &self.public)?;
        map.serialize_field("output_start", &self.output_start)?;
        map.serialize_field("output_index", &self.output.to_string())?;
        map.serialize_field("wrap_anchor", &self.wrap_anchor)?;
        map.serialize_field("program_date_time", &self.pdt)?;
        map.end()
    }
}
impl Serialize for TimelineOutputReport {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = s.serialize_struct("TimelineOutputReport", 6)?;
        map.serialize_field("index", &self.index.to_string())?;
        map.serialize_field("actual_range", &self.range)?;
        map.serialize_field("reason", &format!("{:?}", self.reason))?;
        map.serialize_field("bytes_written", &self.media.bytes_written.to_string())?;
        map.serialize_field("mappings", &self.mappings)?;
        #[derive(Serialize)]
        struct Track {
            codec: String,
            timescale: u32,
            duration: String,
            sample_count: String,
        }
        let tracks: Vec<_> = self
            .media
            .tracks
            .iter()
            .map(|t| Track {
                codec: format!("{:?}", t.codec),
                timescale: t.timescale,
                duration: t.duration.to_string(),
                sample_count: t.sample_count.to_string(),
            })
            .collect();
        map.serialize_field("tracks", &tracks)?;
        map.end()
    }
}
impl Serialize for TimelineSessionReport {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = s.serialize_struct("TimelineSessionReport", 12)?;
        map.serialize_field("schema_version", &1u32)?;
        map.serialize_field("requested", &self.requested)?;
        map.serialize_field("actual", &self.actual)?;
        map.serialize_field("outputs", &self.outputs)?;
        map.serialize_field("gaps", &self.gaps)?;
        map.serialize_field("dependencies", &self.dependencies)?;
        map.serialize_field("random_access_points", &self.access_points)?;
        map.serialize_field("indexed_resources", &self.indexed_resources.to_string())?;
        map.serialize_field("resource_reads", &self.resource_reads.to_string())?;
        map.serialize_field("source_bytes", &self.source_bytes.to_string())?;
        map.serialize_field(
            "peak_planned_samples",
            &self.peak_planned_samples.to_string(),
        )?;
        map.serialize_field(
            "peak_planned_resources",
            &self.peak_planned_resources.to_string(),
        )?;
        map.end()
    }
}

impl Serialize for TimelineAccessPoint {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = s.serialize_struct("TimelineAccessPoint", 5)?;
        map.serialize_field("slot", &self.slot)?;
        map.serialize_field("sample_index", &self.sample.to_string())?;
        map.serialize_field("source", &self.source)?;
        map.serialize_field("presentation", &self.presentation)?;
        map.serialize_field("output_index", &self.output.to_string())?;
        map.end()
    }
}

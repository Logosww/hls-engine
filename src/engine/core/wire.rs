//! Output diagnostics use decimal strings for every wide integer; not a checkpoint.
use super::*;
use serde::{Serialize, Serializer, ser::SerializeStruct};
macro_rules! fields {
    ($self:ident,$serializer:ident,$name:literal,$($key:literal=>$value:expr),+ $(,)?) => {{
        let mut map=$serializer.serialize_struct($name,[$($key),+].len())?;
        $(map.serialize_field($key,&$value)?;)+
        map.end()
    }};
}
impl Serialize for EngineInputProgress {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        fields!(self,s,"EngineInputProgress","input_id"=>self.input,"total"=>Option::<String>::None,
            "discovered"=>self.discovered.to_string(),"accepted"=>self.accepted.to_string(),"downloaded"=>self.downloaded.to_string(),
            "decrypted"=>self.decrypted.to_string(),"committed"=>self.committed.to_string(),"committed_slot"=>self.watermark)
    }
}
impl Serialize for EngineMapping {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        fields!(self,s,"EngineMapping","input_id"=>self.input,"generation"=>self.generation.to_string(),"epoch"=>self.epoch.to_string(),
            "track_id"=>self.track,"source_start"=>self.source,"presentation_start"=>self.presentation,"output_index"=>self.output.to_string(),"output_start"=>self.output_start,"configuration_id"=>self.configuration,"program_date_time"=>self.pdt)
    }
}
impl Serialize for EngineOutputReport {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
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
        fields!(self,s,"EngineOutputReport","index"=>self.index.to_string(),"collected_bytes"=>self.collected_bytes.to_string(),"classic_index_samples"=>self.classic_index_samples.to_string(),"bytes_written"=>self.media.bytes_written.to_string(),
            "duration"=>self.media.duration.to_string(),"duration_timescale"=>self.media.duration_timescale,"segment_count"=>self.media.segment_count.to_string(),"tracks"=>tracks)
    }
}
impl Serialize for EnginePeaks {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        fields!(self,s,"EnginePeaks","queued_descriptors"=>self.queued.to_string(),"queued_metadata_bytes"=>self.metadata.to_string(),
            "samples"=>self.samples.to_string(),"sample_bytes"=>self.sample_bytes.to_string())
    }
}
impl Serialize for EngineMediaReport {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = s.serialize_struct(
            "EngineMediaReport",
            12 + usize::from(self.aac_repairs.is_some()),
        )?;
        map.serialize_field("schema_version", &1u32)?;
        map.serialize_field("end_reason", &format!("{:?}", self.reason))?;
        map.serialize_field("inputs", &self.inputs)?;
        map.serialize_field("bytes_written", &self.bytes.to_string())?;
        map.serialize_field("duration", &self.duration)?;
        map.serialize_field("requested_range", &self.requested)?;
        map.serialize_field("actual_range", &self.actual)?;
        map.serialize_field("gap_count", &self.gaps.to_string())?;
        map.serialize_field("outputs", &self.outputs)?;
        map.serialize_field("mappings", &self.mappings)?;
        map.serialize_field("history_truncated", &self.truncated)?;
        map.serialize_field("peaks", &self.peaks)?;
        if let Some(repairs) = &self.aac_repairs {
            map.serialize_field("aac_repairs", repairs)?;
        }
        map.end()
    }
}

impl Serialize for TrackMetadata {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        fields!(self,s,"TrackMetadata","language"=>self.language(),"name"=>self.name(),"default"=>self.is_default())
    }
}
impl Serialize for OutputTrackInfo {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        fields!(self,s,"OutputTrackInfo","track_id"=>self.id().get(),"output_index"=>self.output_index().to_string(),"input_id"=>self.input_id(),"kind"=>format!("{:?}",self.kind()),"codec"=>format!("{:?}",self.codec()),"metadata"=>self.metadata(),"timescale"=>self.timescale(),"duration"=>self.duration().to_string(),"sample_count"=>self.sample_count().to_string())
    }
}
impl Serialize for SubtitleCueReport {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        fields!(self,s,"SubtitleCueReport","track_id"=>self.track_id().get(),"identifier"=>self.identifier(),"disposition"=>format!("{:?}",self.disposition()),"output_index"=>self.output_index().to_string(),"start"=>self.start(),"end"=>self.end())
    }
}
impl Serialize for EngineReport {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        fields!(self,s,"EngineReport","schema_version"=>1u32,"configuration_id"=>self.configuration_id(),"media"=>self.media(),"tracks"=>self.tracks(),"track_history_truncated"=>self.track_history_truncated(),"subtitle_reports"=>self.subtitle_reports(),"subtitle_history_truncated"=>self.subtitle_history_truncated())
    }
}

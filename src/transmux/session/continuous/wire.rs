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
impl Serialize for ContinuousInputProgress {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        fields!(self,s,"ContinuousInputProgress","input_id"=>self.input,"total"=>Option::<String>::None,
            "discovered"=>self.discovered.to_string(),"accepted"=>self.accepted.to_string(),"downloaded"=>self.downloaded.to_string(),
            "decrypted"=>self.decrypted.to_string(),"committed"=>self.committed.to_string(),"committed_slot"=>self.watermark)
    }
}
impl Serialize for ContinuousMapping {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        fields!(self,s,"ContinuousMapping","input_id"=>self.input,"generation"=>self.generation.to_string(),"epoch"=>self.epoch.to_string(),
            "track_id"=>self.track,"source_start"=>self.source,"presentation_start"=>self.presentation,"output_index"=>self.output.to_string(),"output_start"=>self.output_start,"configuration_id"=>self.configuration,"program_date_time"=>self.pdt)
    }
}
impl Serialize for ContinuousOutputReport {
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
        fields!(self,s,"ContinuousOutputReport","index"=>self.index.to_string(),"collected_bytes"=>self.collected_bytes.to_string(),"classic_index_samples"=>self.classic_index_samples.to_string(),"bytes_written"=>self.media.bytes_written.to_string(),
            "duration"=>self.media.duration.to_string(),"duration_timescale"=>self.media.duration_timescale,"segment_count"=>self.media.segment_count.to_string(),"tracks"=>tracks)
    }
}
impl Serialize for ContinuousPeaks {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        fields!(self,s,"ContinuousPeaks","queued_descriptors"=>self.queued.to_string(),"queued_metadata_bytes"=>self.metadata.to_string(),
            "samples"=>self.samples.to_string(),"sample_bytes"=>self.sample_bytes.to_string())
    }
}
impl Serialize for ContinuousReport {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        fields!(self,s,"ContinuousReport","schema_version"=>1u32,"end_reason"=>format!("{:?}",self.reason),"inputs"=>self.inputs,
            "bytes_written"=>self.bytes.to_string(),"duration"=>self.duration,"requested_range"=>self.requested,"actual_range"=>self.actual,"gap_count"=>self.gaps.to_string(),"outputs"=>self.outputs,
            "mappings"=>self.mappings,"history_truncated"=>self.truncated,"peaks"=>self.peaks)
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
impl Serialize for MultiTrackReport {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        fields!(self,s,"MultiTrackReport","schema_version"=>1u32,"configuration_id"=>self.configuration_id(),"media"=>self.media(),"tracks"=>self.tracks(),"track_history_truncated"=>self.track_history_truncated(),"subtitle_reports"=>self.subtitle_reports(),"subtitle_history_truncated"=>self.subtitle_history_truncated())
    }
}

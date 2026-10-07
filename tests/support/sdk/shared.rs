//! Integration extension compiled inside the real SDK shared host module.
use super::*;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub range: Option<PresentationRange>,
    pub collapse: bool,
    pub split: bool,
}
impl Selection {
    pub fn options(&self) -> TimelinePrepareOptions {
        let mut options = TimelinePrepareOptions::default()
            .with_gap_policy(if self.collapse {
                GapPolicy::Collapse
            } else {
                GapPolicy::Preserve
            })
            .with_change_policy(if self.split {
                TimelineChangePolicy::Split
            } else {
                TimelineChangePolicy::Fail
            });
        if let Some(range) = self.range {
            options = options.with_range(range);
        }
        options
    }
}
pub fn timeline_failure(error: TimelineSessionError) -> Value {
    json!({"error":{"code":format!("{:?}",error.kind()),"completed":error.completed_outputs()}})
}
pub async fn prepare_timeline(
    r: &Request,
    host: Arc<dyn Host>,
    options: TimelinePrepareOptions,
) -> std::result::Result<TimelinePreparedTransmux, Value> {
    if r.wire_version != 2 {
        return Err(error("MANIFEST_INVALID", "wireVersion"));
    }
    let source = Arc::new(SourceHost {
        host: host.clone(),
        cap: r.limits.resource_bytes,
        ids: Mutex::new(Vec::new()),
        next: Arc::new(AtomicU32::new(1)),
    });
    let mut inputs = KeyedInputs::new(KeyedInput::new(
        parse(&r.primary, "primary")?,
        source.clone(),
    ));
    if let Some(a) = &r.audio {
        inputs = inputs.with_audio(KeyedInput::new(parse(a, "audio")?, source));
    }
    let keys = KeySession::new(
        r.operation_id.clone(),
        r.scope.clone(),
        Arc::new(Provider(host.clone())),
        Arc::new(Clock(host)),
        KeySessionOptions::default()
            .with_limits(
                r.limits.key_requests,
                r.limits.cached_keys,
                r.limits.key_waiters,
            )
            .with_formats(
                r.key_formats
                    .iter()
                    .map(|f| KeyFormatSupport::new(&f.format, f.versions.clone()))
                    .collect(),
            ),
    )
    .map_err(|_| error("KEY_INVALID", "options"))?;
    let resources = ResourceOptions::default()
        .with_limits(
            r.limits.resource_bytes,
            r.limits.waiting_bytes,
            r.limits.resources,
        )
        .with_encrypted_ranges(if r.encrypted_ranges == "complete-resources" {
            EncryptedRangePolicy::CompleteResources
        } else {
            EncryptedRangePolicy::Reject
        });
    prepare_hls_timeline(inputs, keys, options.with_resources(resources))
        .await
        .map_err(timeline_failure)
}

/// v0.9 integration extension: keep the SDK's actual SourceHost and key provider.
pub fn prepare_continuous(r:&Request,host:Arc<dyn Host>)->std::result::Result<ContinuousSession,Value> {
    let source=Arc::new(SourceHost {host:host.clone(),cap:r.limits.resource_bytes,ids:Mutex::new(Vec::new()),next:Arc::new(AtomicU32::new(1))});
    let keys=KeySession::new(r.operation_id.clone(),r.scope.clone(),Arc::new(Provider(host.clone())),Arc::new(Clock(host)),KeySessionOptions::default()).map_err(|_|error("KEY_INVALID","options"))?;
    let text=r.primary.text.lines().filter(|l|!l.starts_with("#EXT-X-ENDLIST")&&!l.starts_with("#EXT-X-PLAYLIST-TYPE")).collect::<Vec<_>>().join("\n");
    let mut initial=String::new();let mut count=0;
    for line in text.lines() {initial.push_str(line);initial.push('\n');if !line.starts_with('#')&&!line.is_empty(){count+=1;if count==2{break;}}}
    let id=InputId::new("primary").unwrap();
    let parse_open=|content:String,revision|parse_playlist_snapshot(&TextResource{location:SourceLocation::Url(url::Url::parse(&r.primary.url).unwrap()),content},PlaylistContext::new(id.clone(),0).with_revision(revision)).unwrap();
    let first=parse_open(initial,0);let full=parse_open(text,1);
    let holder=Arc::new(Mutex::new(None::<ContinuousHandle>));let callback=holder.clone();let input=id.clone();
    let options=ContinuousOptions::default().with_resources(ResourceOptions::default().with_encrypted_ranges(EncryptedRangePolicy::CompleteResources)).with_on_event(Arc::new(move|event| {
        if let ContinuousEvent::Committed {input:progress,..}=event {
            let h=callback.lock().unwrap();let h=h.as_ref().unwrap();
            if progress.committed()==1 {h.accept_snapshot(&input,&full).unwrap();}else{h.stop();}
        }
    }));
    let session=ContinuousSession::new(ContinuousInputs::new(ContinuousInput::new(id.clone(),source)),keys,options).map_err(|_|error("SESSION_INVALID","options"))?;
    let handle=session.handle();*holder.lock().unwrap()=Some(handle.clone());handle.accept_snapshot(&id,&first).unwrap();Ok(session)
}
pub fn continuous_report(report:ContinuousReport)->Value {
    let mut value=serde_json::to_value(report).unwrap();value.as_object_mut().unwrap().remove("peaks");value
}

// v0.10 isolated adapter extension. Immutable selected inputs, real SourceHost/provider.
struct MultiWait;
impl ContinuousWait for MultiWait {
    #[cfg(not(target_arch="wasm32"))]
    fn wait(&self,_:std::time::Duration)->std::pin::Pin<Box<dyn std::future::Future<Output=()>+Send+'_>> {Box::pin(std::future::pending())}
    #[cfg(target_arch="wasm32")]
    fn wait(&self,_:std::time::Duration)->std::pin::Pin<Box<dyn std::future::Future<Output=()>+'_>> {Box::pin(std::future::pending())}
}
pub fn prepare_multitrack(r:&Request,host:Arc<dyn Host>)->std::result::Result<MultiTrackSession,Value> {
    let source=Arc::new(SourceHost{host:host.clone(),cap:r.limits.resource_bytes,ids:Mutex::new(Vec::new()),next:Arc::new(AtomicU32::new(1))});
    let keys=KeySession::new(r.operation_id.clone(),r.scope.clone(),Arc::new(Provider(host.clone())),Arc::new(Clock(host)),KeySessionOptions::default()).map_err(|_|error("KEY_INVALID","options"))?;
    let id=|s|InputId::new(s).unwrap();
    let mut inputs=MultiTrackInputs::new(ContinuousInput::new(id("primary"),source.clone()),if r.audio.is_some(){EmbeddedAudio::Exclude}else{EmbeddedAudio::Keep});
    if r.audio.is_some(){for (name,lang) in [("en","en"),("ja","ja")]{inputs=inputs.with_audio(ContinuousInput::new(id(name),source.clone()),TrackMetadata::new(lang,name));}}
    inputs=inputs.with_subtitle(SubtitleTrack::new(id("cc"),id("primary"),TrackMetadata::new("en","Captions")));
    let s=MultiTrackSession::new(inputs,keys,ContinuousOptions::default().with_mode(ContinuousMode::Vod).with_waiter(Arc::new(MultiWait),std::time::Duration::from_secs(1))).map_err(|_|error("SESSION_INVALID","options"))?;
    let h=s.handle();
    let primary=parse_playlist_snapshot(&TextResource{location:SourceLocation::Url(url::Url::parse(&r.primary.url).unwrap()),content:r.primary.text.clone()},PlaylistContext::new(id("primary"),0)).unwrap();
    h.accept_snapshot(&id("primary"),&primary).unwrap();
    if let Some(a)=&r.audio{for name in ["en","ja"]{let snap=parse_playlist_snapshot(&TextResource{location:SourceLocation::Url(url::Url::parse(&a.url).unwrap()),content:a.text.clone()},PlaylistContext::new(id(name),0)).unwrap();h.accept_snapshot(&id(name),&snap).unwrap();}}
    let cc=h.subtitle_track_id(&id("cc")).unwrap();h.accept_cues(cc,&[SubtitleCue::new(0,0,MediaTime::new(0,1000).unwrap(),MediaTime::new(1000,1000).unwrap(),"SDK caption").with_settings("align:start")]).unwrap();h.end_subtitles(cc).unwrap();Ok(s)
}
pub fn multitrack_report(report:MultiTrackReport)->Value {
    let mut value=serde_json::to_value(report).unwrap();value["media"].as_object_mut().unwrap().remove("peaks");value
}

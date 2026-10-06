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

use super::*;
use crate::crypto::key::KeyFuture;
use std::task::Poll;
use zeroize::Zeroizing;

type DownloadResult = ResourceResult<Zeroizing<Vec<u8>>>;
enum DownloadState {
    // The mutex makes the scheduler Sync while Engine is borrowed by output
    // callbacks. Only the owning writer polls these futures, through &mut self.
    Pending(std::sync::Mutex<KeyFuture<DownloadResult>>),
    Ready(DownloadResult),
}
struct Download {
    descriptor: SegmentDescriptor,
    reserved: u64,
    state: DownloadState,
}

/// Speculative transport state is owned by the writer future and never archived.
/// Descriptors stay in the admission queue until normal ordered consumption.
#[derive(Default)]
pub(super) struct Downloads {
    slots: Vec<Download>,
}
impl Downloads {
    fn reserved(&self) -> u64 {
        self.slots.iter().map(|slot| slot.reserved).sum()
    }
    fn enqueue(
        &mut self,
        session: &SessionCore,
        input: usize,
        descriptor: SegmentDescriptor,
        reserved: u64,
    ) -> EngineResult<()> {
        let request =
            ResourceRequest::from_descriptor(descriptor.clone(), false).map_err(resource_error)?;
        let resources = session.resources.clone();
        let source = session.sources[input].0.clone();
        self.slots.push(Download {
            descriptor,
            reserved,
            state: DownloadState::Pending(std::sync::Mutex::new(Box::pin(async move {
                resources.download(source, &request).await
            }))),
        });
        Ok(())
    }
    pub(super) async fn read(
        &mut self,
        session: &SessionCore,
        input: usize,
        descriptor: &SegmentDescriptor,
    ) -> EngineResult<Zeroizing<Vec<u8>>> {
        let limit = session.options.limits.download_bytes;
        if !self.slots.iter().any(|slot| slot.descriptor == *descriptor) {
            let request = ResourceRequest::from_descriptor(descriptor.clone(), false)
                .map_err(resource_error)?;
            let reserved = session
                .options
                .resources
                .request_size(&request)
                .map_err(resource_error)?;
            // A different input may need the window currently occupied by
            // speculative reads. Demand always takes priority and cannot wait
            // for consumption of another lane's buffered media.
            while self.slots.len() >= session.options.download_concurrency
                || reserved > limit - self.reserved()
            {
                self.slots.pop();
            }
            self.enqueue(session, input, descriptor.clone(), reserved)?;
        }
        // Keep full immutable descriptors as identities, including generation,
        // encryption declarations and byte ranges. Mutable URLs are insufficient.
        let candidates = {
            let state = session.shared.inner.lock().unwrap();
            state.lanes[input]
                .queue
                .iter()
                .take(session.options.download_concurrency)
                .cloned()
                .collect::<Vec<_>>()
        };
        for candidate in candidates {
            if self.slots.len() >= session.options.download_concurrency {
                break;
            }
            if candidate.gap() || self.slots.iter().any(|slot| slot.descriptor == candidate) {
                continue;
            }
            let request = ResourceRequest::from_descriptor(candidate.clone(), false)
                .map_err(resource_error)?;
            // A future descriptor's admission error belongs to its consumption
            // position, so it must not prevent earlier media from committing.
            let Ok(reserved) = session.options.resources.request_size(&request) else {
                break;
            };
            if reserved > limit - self.reserved() {
                break;
            }
            self.enqueue(session, input, candidate, reserved)?;
        }
        std::future::poll_fn(|cx| {
            for slot in &mut self.slots {
                if let DownloadState::Pending(future) = &mut slot.state
                    && let Poll::Ready(result) = future.get_mut().unwrap().as_mut().poll(cx)
                {
                    slot.state = DownloadState::Ready(result);
                }
            }
            let index = self
                .slots
                .iter()
                .position(|slot| slot.descriptor == *descriptor)
                .expect("demanded download is retained");
            if matches!(self.slots[index].state, DownloadState::Ready(_)) {
                let DownloadState::Ready(result) = self.slots.remove(index).state else {
                    unreachable!()
                };
                Poll::Ready(result.map_err(resource_error))
            } else {
                Poll::Pending
            }
        })
        .await
    }
}

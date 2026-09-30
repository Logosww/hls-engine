//! Manual prefetch RSS benchmark; run via scripts/benchmark_prefetch.py.
#[cfg(feature = "default-source")]
#[tokio::main]
async fn main() -> hls_transmux::Result<()> {
    use hls_transmux::{HttpRequestPolicy, ReqwestSource, Source, SourceLocation};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let args: Vec<_> = std::env::args().collect();
    let size: usize = args[1].parse().unwrap();
    let concurrency: usize = args[2].parse().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let root = format!("http://{}", listener.local_addr()?);
    let complete = Arc::new(AtomicUsize::new(0));
    let count = complete.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let count = count.clone();
            tokio::spawn(async move {
                let mut request = [0; 4096];
                let n = socket.read(&mut request).await.unwrap();
                if String::from_utf8_lossy(&request[..n]).starts_with("GET /playlist") {
                    let mut body = String::from("#EXTM3U\n");
                    for i in 0..32 {
                        body.push_str(&format!("#EXTINF:1,\ns{i}\n"));
                    }
                    body.push_str("#EXT-X-ENDLIST\n");
                    let _ = socket.write_all(format!("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await;
                } else {
                    if socket.write_all(format!("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {size}\r\n\r\n").as_bytes()).await.is_err() { return; }
                    let block = [1u8; 65536];
                    let mut remaining = size;
                    while remaining > 0 {
                        let n = remaining.min(block.len());
                        if socket.write_all(&block[..n]).await.is_err() {
                            return;
                        }
                        remaining -= n;
                    }
                    count.fetch_add(1, Ordering::SeqCst);
                }
            });
        }
    });
    let mut policy = HttpRequestPolicy::default();
    policy.max_resource_bytes = Some(size as u64);
    let source = ReqwestSource::with_concurrency(concurrency).with_request_policy(policy);
    source
        .read_text(&SourceLocation::Url(
            url::Url::parse(&format!("{root}/playlist")).unwrap(),
        ))
        .await?;
    let body;
    let expected = if concurrency == 1 {
        body = Some(
            source
                .read_bytes(
                    &SourceLocation::Url(url::Url::parse(&format!("{root}/s0")).unwrap()),
                    None,
                )
                .await?,
        );
        1
    } else {
        body = None;
        concurrency * 3
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while complete.load(Ordering::SeqCst) < expected {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    println!(
        "{{\"resource_bytes\":{size},\"concurrency\":{concurrency},\"completed_requests\":{},\"buffered_payload_bytes\":{}}}",
        complete.load(Ordering::SeqCst),
        size * expected
    );
    source.stop_session();
    drop(body);
    server.abort();
    Ok(())
}
#[cfg(not(feature = "default-source"))]
fn main() {}

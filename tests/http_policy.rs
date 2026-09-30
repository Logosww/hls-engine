#![cfg(feature = "default-source")]
use hls_transmux::{ByteRange, HttpRequestPolicy, ReqwestSource, Source, SourceLocation};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Server {
    location: SourceLocation,
    requests: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn start_server(replies: Vec<String>, body_delay: Duration) -> Server {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let location = SourceLocation::Url(
        url::Url::parse(&format!(
            "http://{}/media?signature=secret#private",
            listener.local_addr().unwrap()
        ))
        .unwrap(),
    );
    let requests = Arc::new(AtomicUsize::new(0));
    let counter = requests.clone();
    let task = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let counter = counter.clone();
            let replies = replies.clone();
            tokio::spawn(async move {
                let mut request = Vec::new();
                loop {
                    let mut buf = [0; 1024];
                    let n = stream.read(&mut buf).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    request.extend_from_slice(&buf[..n]);
                    if request.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                if String::from_utf8_lossy(&request).starts_with("GET /playlist") {
                    let body = "#EXTM3U\n#EXTINF:1,\nmedia?signature=secret\n#EXT-X-ENDLIST\n";
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(reply.as_bytes()).await;
                    return;
                }
                let index = counter
                    .fetch_add(1, Ordering::SeqCst)
                    .min(replies.len() - 1);
                let (headers, body) = replies[index].split_once("\r\n\r\n").unwrap();
                let _ = stream
                    .write_all(format!("{headers}\r\n\r\n").as_bytes())
                    .await;
                tokio::time::sleep(body_delay).await;
                let _ = stream.write_all(body.as_bytes()).await;
            });
        }
    });
    Server {
        location,
        requests,
        task,
    }
}
fn response(status: &str, extra: &str, body: &str) -> String {
    format!("HTTP/1.1 {status}\r\nConnection: close\r\n{extra}\r\n{body}")
}
async fn read(
    source: &ReqwestSource,
    server: &Server,
    concurrency: usize,
    range: Option<&ByteRange>,
) -> hls_transmux::Result<Vec<u8>> {
    if concurrency > 1 {
        let SourceLocation::Url(mut url) = server.location.clone() else {
            unreachable!()
        };
        url.set_path("/playlist");
        url.set_query(None);
        url.set_fragment(None);
        source.read_text(&SourceLocation::Url(url)).await.unwrap();
    }
    source.read_bytes(&server.location, range).await
}
#[tokio::test]
async fn validates_content_range_and_actual_length() {
    let range = ByteRange {
        offset: 10,
        length: 3,
    };
    for (extra, body, good) in [
        (
            "Content-Range: bytes 10-12/20\r\nContent-Length: 3\r\n",
            "abc",
            true,
        ),
        (
            "Content-Range: bytes 11-13/20\r\nContent-Length: 3\r\n",
            "abc",
            false,
        ),
        (
            "Content-Range: bytes 10-12/12\r\nContent-Length: 3\r\n",
            "abc",
            false,
        ),
        ("Content-Range: bytes 10-12/*\r\n", "ab", false),
        ("Content-Range: bytes 10-12/*\r\n", "abcd", false),
        ("Content-Length: 3\r\n", "abc", false),
    ] {
        let server = start_server(
            vec![response("206 Partial Content", extra, body)],
            Duration::ZERO,
        )
        .await;
        let result = read(&ReqwestSource::new(), &server, 1, Some(&range)).await;
        assert_eq!(result.is_ok(), good, "{result:?}");
        if let Err(error) = result {
            let text = error.to_string();
            assert!(!text.contains("secret"));
            assert!(!text.contains("private"));
        }
    }
}
#[tokio::test]
async fn retries_are_bounded_in_serial_and_prefetch_paths() {
    for concurrency in [1, 2] {
        let server = start_server(
            vec![
                response("503 Unavailable", "Content-Length: 0\r\n", ""),
                response("200 OK", "Content-Length: 3\r\n", "abc"),
            ],
            Duration::ZERO,
        )
        .await;
        let mut policy = HttpRequestPolicy::default();
        policy.max_retries = 1;
        policy.backoff_base = Duration::ZERO;
        let source =
            ReqwestSource::with_concurrency(concurrency).with_request_policy(policy.clone());
        assert_eq!(
            read(&source, &server, concurrency, None).await.unwrap(),
            b"abc"
        );
        assert_eq!(server.requests.load(Ordering::SeqCst), 2);
        let server = start_server(
            vec![response("503 Unavailable", "Content-Length: 0\r\n", "")],
            Duration::ZERO,
        )
        .await;
        let source = ReqwestSource::with_concurrency(concurrency).with_request_policy(policy);
        assert!(read(&source, &server, concurrency, None).await.is_err());
        assert_eq!(server.requests.load(Ordering::SeqCst), 2);
    }
}
#[tokio::test]
async fn timeout_includes_body_and_limits_cover_chunked_bodies() {
    let server = start_server(
        vec![response("200 OK", "Content-Length: 3\r\n", "abc")],
        Duration::from_secs(2),
    )
    .await;
    let mut policy = HttpRequestPolicy::default();
    policy.request_timeout = Some(Duration::from_millis(50));
    let source = ReqwestSource::new().with_request_policy(policy);
    assert!(
        tokio::time::timeout(
            Duration::from_secs(1),
            source.read_bytes(&server.location, None)
        )
        .await
        .unwrap()
        .unwrap_err()
        .to_string()
        .contains("timed out")
    );
    for extra in ["Content-Length: 4\r\n", ""] {
        let server = start_server(vec![response("200 OK", extra, "abcd")], Duration::ZERO).await;
        let mut policy = HttpRequestPolicy::default();
        policy.max_resource_bytes = Some(3);
        assert!(
            ReqwestSource::new()
                .with_request_policy(policy)
                .read_bytes(&server.location, None)
                .await
                .is_err()
        );
    }
}
#[tokio::test]
async fn malformed_range_and_permanent_status_never_retry() {
    for status in ["404 Not Found", "206 Partial Content"] {
        let server = start_server(
            vec![response(status, "Content-Length: 3\r\n", "abc")],
            Duration::ZERO,
        )
        .await;
        let mut policy = HttpRequestPolicy::default();
        policy.max_retries = 5;
        assert!(
            ReqwestSource::new()
                .with_request_policy(policy)
                .read_bytes(
                    &server.location,
                    Some(&ByteRange {
                        offset: 1,
                        length: 3
                    })
                )
                .await
                .is_err()
        );
        assert_eq!(server.requests.load(Ordering::SeqCst), 1);
    }
}

#[derive(Debug)]
struct Cancel(tokio::sync::watch::Sender<bool>);
impl hls_transmux::CancelToken for Cancel {
    fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }
    fn cancelled(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            let mut signal = self.0.subscribe();
            while !*signal.borrow_and_update() {
                if signal.changed().await.is_err() {
                    break;
                }
            }
        })
    }
}
#[tokio::test]
async fn cancellation_interrupts_prefetch_retry_backoff() {
    use hls_transmux::{
        Error, HlsInput, OutputFormat, TransmuxOptions, transmux_hls_to_writer_async,
    };
    let server = start_server(
        vec![response("503 Unavailable", "Content-Length: 0\r\n", "")],
        Duration::ZERO,
    )
    .await;
    let SourceLocation::Url(mut root) = server.location.clone() else {
        unreachable!()
    };
    root.set_path("/playlist");
    root.set_query(None);
    root.set_fragment(None);
    let mut policy = HttpRequestPolicy::default();
    policy.max_retries = 3;
    policy.backoff_base = Duration::from_secs(5);
    let source = Arc::new(ReqwestSource::with_concurrency(2).with_request_policy(policy));
    let (signal, _) = tokio::sync::watch::channel(false);
    let token = Arc::new(Cancel(signal.clone()));
    let count = server.requests.clone();
    let stopper = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(1), async {
            while count.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        signal.send_replace(true);
    });
    let mut bytes = Vec::new();
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        transmux_hls_to_writer_async(
            HlsInput::custom(source, SourceLocation::Url(root)),
            &mut bytes,
            TransmuxOptions {
                output_format: OutputFormat::FragmentedMp4,
                cancel: Some(token),
                ..Default::default()
            },
        ),
    )
    .await
    .unwrap();
    stopper.await.unwrap();
    assert!(matches!(result, Err(Error::Cancelled)));
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn prefetch_enforces_response_size_limit() {
    let body = "x".repeat(129);
    let server = start_server(vec![response("200 OK", "", &body)], Duration::ZERO).await;
    let mut policy = HttpRequestPolicy::default();
    policy.max_resource_bytes = Some(128);
    let source = ReqwestSource::with_concurrency(2).with_request_policy(policy);
    assert!(
        read(&source, &server, 2, None)
            .await
            .unwrap_err()
            .to_string()
            .contains("size limit")
    );
}

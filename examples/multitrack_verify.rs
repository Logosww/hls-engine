#[allow(dead_code)]
#[path = "../tests/support/sample_crypto.rs"]
mod sample_corpus;
#[path = "../tests/support/multitrack_runtime.rs"]
mod suite;
#[tokio::main(flavor = "current_thread")]
async fn main() {
    println!(
        "{}",
        suite::run(std::sync::Arc::new(sample_corpus::Provider)).await
    );
}

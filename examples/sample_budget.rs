#[path = "../tests/support/allocation.rs"]
mod allocation;
#[path = "../tests/support/sample_crypto.rs"]
#[allow(dead_code)]
mod sample_corpus;
#[path = "../tests/support/sample_profile.rs"]
mod sample_profile;
#[tokio::main(flavor = "current_thread")]
async fn main() {
    println!(
        "{}",
        sample_profile::run(std::sync::Arc::new(sample_corpus::Provider)).await
    );
}

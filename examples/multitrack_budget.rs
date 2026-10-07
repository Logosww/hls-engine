#[path = "../tests/support/allocation.rs"]
mod allocation;
#[path = "../tests/support/multitrack_profile.rs"]
mod profile;
#[allow(dead_code)]
#[path = "../tests/support/sample_crypto.rs"]
mod sample_corpus;
#[tokio::main(flavor = "current_thread")]
async fn main() {
    println!("{}", profile::run().await);
}

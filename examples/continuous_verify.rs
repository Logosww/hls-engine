#[path = "../tests/support/continuous_runtime.rs"]
mod continuous;
#[allow(dead_code)]
#[path = "../tests/support/sample_crypto.rs"]
mod sample_corpus;
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let value = continuous::run(std::sync::Arc::new(sample_corpus::Provider)).await;
    println!("{}", serde_json::to_string_pretty(&value).unwrap());
}

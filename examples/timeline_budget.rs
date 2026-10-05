//! Allocation and first-write measurements, including finite snapshot/catalog/report costs.
#[path = "../tests/support/allocation.rs"]
mod allocation;
#[path = "../tests/support/timeline_profile.rs"]
mod profile;
#[path = "../tests/support/timeline_budget.rs"]
mod timeline_budget;
#[tokio::main(flavor = "current_thread")]
async fn main() {
    println!("{}", profile::run().await);
}

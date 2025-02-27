#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    openai_limiter::run().await
}

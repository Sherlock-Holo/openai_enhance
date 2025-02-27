#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    openai_enhance::run().await
}

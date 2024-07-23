use alloy::providers::{Provider, ProviderBuilder};
use std::time::{SystemTime, UNIX_EPOCH};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let start = SystemTime::now();

    // 转换为 Unix 时间戳
    match start.duration_since(UNIX_EPOCH) {
        Ok(duration) => {
            println!("Seconds since the Unix epoch: {}", duration.as_secs());
            println!(
                "Milliseconds since the Unix epoch: {}",
                duration.as_millis()
            );
            println!("Nanoseconds since the Unix epoch: {}", duration.as_nanos());
        }
        Err(e) => {
            println!("Error: {:?}", e);
        }
    }

    let provider = ProviderBuilder::new().on_http("http://localhost:8545".parse()?);

    let block = provider.get_block_number().await;

    println!("Block Number: {:?}", block);

    Ok(())
}

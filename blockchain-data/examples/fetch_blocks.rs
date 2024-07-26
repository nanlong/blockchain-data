use blockchain_data::{Ethereum, EthereumConfig};
use futures::StreamExt;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = EthereumConfig::builder()
        .rpc_url("http://localhost:8545".parse()?)
        .ws_url("ws://localhost:8545".parse()?)
        .build()?;

    let client = Arc::new(Ethereum::try_new(config).await?);

    let mut stream = client.fetch_blocks(1, 20).await?;

    while let Some(Ok(block)) = stream.next().await {
        println!("Block Number: {:?}", block.header.number);
    }

    Ok(())
}

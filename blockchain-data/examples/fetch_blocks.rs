use alloy::eips::BlockNumberOrTag;
use blockchain_data::{Ethereum, EthereumConfig};
use futures::{pin_mut, StreamExt};
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = EthereumConfig::builder()
        .rpc_url("http://localhost:8545".parse()?)
        .ws_url("ws://localhost:8545".parse()?)
        .build()?;

    let client = Arc::new(Ethereum::try_new(config).await?);

    let stream = client.fetch_blocks(1, BlockNumberOrTag::Latest).await?;

    pin_mut!(stream);

    while let Some(block) = stream.next().await {
        println!("Block Number: {:?}", block.header.number);
    }

    Ok(())
}

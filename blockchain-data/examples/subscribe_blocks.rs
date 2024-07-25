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
    let subscribe_client = client.clone();
    let watch_client = client.clone();
    let update_client = client.clone();

    let subscribe_task = tokio::spawn(async move {
        let stream = subscribe_client.subscribe_blocks().await?;

        pin_mut!(stream);

        while let Some(Ok(block)) = stream.next().await {
            println!("Subscribe Block Number: {:?}", block.header.number);
        }

        Ok::<(), anyhow::Error>(())
    });

    let watch_task = tokio::spawn(async move {
        let stream = watch_client.watch_blocks().await?;

        pin_mut!(stream);

        while let Some(ret) = stream.next().await {
            match ret {
                Ok(block) => println!("Watch Block Number: {:?}", block.header.number),
                Err(e) => println!("Watch Block Error: {:?}", e),
            }
        }

        Ok::<(), anyhow::Error>(())
    });

    tokio::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;

        update_client
            .update_ws_provider("ws://localhost:8545")
            .await?;

        Ok::<(), anyhow::Error>(())
    });

    tokio::select! {
        _ = subscribe_task => {},
        _ = watch_task => {},
    }

    Ok(())
}

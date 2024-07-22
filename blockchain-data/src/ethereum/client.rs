use super::EthereumConfig;
use alloy::{
    primitives::BlockNumber,
    providers::{Provider, ProviderBuilder, RootProvider, WsConnect},
    pubsub::PubSubFrontend,
    rpc::types::{Block, BlockTransactionsKind},
    transports::http::{Client, Http},
};
use anyhow::Result;
use async_stream::stream;
use futures::{Stream, StreamExt};
use std::{sync::Arc, time::Duration};
use tokio::{sync::Mutex, time::sleep};

// Ethereum is a struct that represents an Ethereum client.
pub struct Ethereum {
    http_provider: Option<RootProvider<Http<Client>>>,
    ws_provider: Option<RootProvider<PubSubFrontend>>,
    latest_block: Arc<Mutex<BlockNumber>>,
}

impl Ethereum {
    pub async fn try_new(config: EthereumConfig) -> Result<Self> {
        // Create the HTTP provider
        let http_provider = config
            .rpc_url
            .map(|rpc_url| ProviderBuilder::new().on_http(rpc_url));

        // Create the Websocket provider
        let ws_provider = if let Some(ws_url) = config.ws_url {
            let ws = WsConnect::new(ws_url);
            let provider = ProviderBuilder::new().on_ws(ws).await?;
            Some(provider)
        } else {
            None
        };

        let latest_block = Arc::new(Mutex::new(BlockNumber::MIN));
        let latest_block_cloned = latest_block.clone();

        let ethereum = Ethereum {
            http_provider,
            ws_provider,
            latest_block,
        };

        if config.subscribe_latest_block {
            let mut block_stream = ethereum.subscribe_blocks().await.unwrap();

            tokio::spawn(async move {
                while let Some(block) = block_stream.next().await {
                    let block_number = block.header.number.unwrap();
                    let mut latest_block_guard = latest_block_cloned.lock().await;
                    *latest_block_guard = block_number;
                }
            });
        }

        Ok(ethereum)
    }

    pub async fn subscribe_blocks(&self) -> Result<impl Stream<Item = Block>> {
        if self.ws_provider.is_none() {
            return Err(anyhow::anyhow!("Websocket provider not configured"));
        }

        // Subscribe to new blocks
        let subscription = self
            .ws_provider
            .as_ref()
            .unwrap()
            .subscribe_blocks()
            .await?;

        // Return the stream of blocks
        Ok(subscription.into_stream())
    }

    pub async fn watch_blocks(&self) -> Result<impl Stream<Item = Block> + '_> {
        if self.http_provider.is_none() {
            return Err(anyhow::anyhow!("HTTP provider not configured"));
        }

        let client = self.http_provider.as_ref().unwrap();

        // Create a stream of block hashes
        let poller = client.watch_blocks().await?;
        let mut block_hash_stream = poller.into_stream();

        // Convert the stream of block hashes into a stream of blocks
        let block_stream = stream! {
            while let Some(block_hashes) = block_hash_stream.next().await {
                for block_hash in block_hashes {
                    let ret = client.get_block_by_hash(block_hash, BlockTransactionsKind::Full).await;

                    if let Ok(Some(block)) = ret {
                        yield block;
                    } else {
                        println!("Failed to get block by hash: {:?}", block_hash);
                    }
                }
            }
        };

        // Return the stream of blocks
        Ok(block_stream)
    }

    pub async fn fetch_blocks(
        &self,
        start: BlockNumber,
        confirmations: BlockNumber,
    ) -> Result<impl Stream<Item = Block> + '_> {
        if self.http_provider.is_none() {
            return Err(anyhow::anyhow!("HTTP provider not configured"));
        }

        let client = self.http_provider.as_ref().unwrap();
        let latest_block_cloned = self.latest_block.clone();
        let mut current_block = start;

        // Create a stream of blocks
        let block_stream = stream! {
            loop {
                let latest_block = *latest_block_cloned.lock().await;

                if latest_block < confirmations || current_block > latest_block - confirmations {
                    sleep(Duration::from_millis(1000)).await;
                } else {
                    let ret = client.get_block_by_number(current_block.into(), false).await;

                    if let Ok(Some(block)) = ret {
                        yield block;
                        current_block += 1;
                    } else {
                        println!("Failed to get block by number: {:?}", current_block);
                    }

                    sleep(Duration::from_millis(100)).await;
                };
            }
        };

        // Return the stream of blocks
        Ok(block_stream)
    }
}

use std::time::Duration;

use crate::ethereum::config::EthereumConfig;
use alloy::{
    eips::BlockNumberOrTag,
    primitives::BlockNumber,
    providers::{Provider, ProviderBuilder, RootProvider, WsConnect},
    pubsub::PubSubFrontend,
    rpc::types::{Block, BlockTransactionsKind},
    transports::http::{Client, Http},
};
use anyhow::Result;
use async_stream::stream;
use futures::{Stream, StreamExt};
use tokio::{sync::mpsc, time::sleep};

// Ethereum is a struct that represents an Ethereum client.
pub struct Ethereum {
    http_provider: Option<RootProvider<Http<Client>>>,
    ws_provider: Option<RootProvider<PubSubFrontend>>,
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

        Ok(Ethereum {
            http_provider,
            ws_provider,
        })
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
        start_block: BlockNumber,
        end_block: impl Into<BlockNumberOrTag>,
    ) -> Result<impl Stream<Item = Block> + '_> {
        if self.http_provider.is_none() {
            return Err(anyhow::anyhow!("HTTP provider not configured"));
        }

        if self.ws_provider.is_none() {
            return Err(anyhow::anyhow!("Websocket provider not configured"));
        }

        // Convert the end block into a block number
        // and validate that it is a valid block number
        // or 'latest'
        // if it is invalid, return an error
        let end_block = end_block.into();

        if !end_block.is_number() && !end_block.is_latest() {
            return Err(anyhow::anyhow!(
                "Invalid end block, must be a number or 'latest'"
            ));
        }

        let (tx, mut rx) = mpsc::channel::<Block>(1);
        let mut block_stream = self.subscribe_blocks().await.unwrap();

        tokio::spawn(async move {
            while let Some(block) = block_stream.next().await {
                if tx.send(block).await.is_err() {
                    break;
                }
            }
        });

        let client = self.http_provider.as_ref().unwrap();
        let latest_block = client.get_block_number().await?;
        let mut end_block_num;
        let mut current_block = start_block;

        // Get the latest block number
        if end_block.is_number() {
            end_block_num = end_block.as_number().unwrap();
        } else {
            end_block_num = latest_block;
        }

        // Create a stream of blocks
        let block_stream = stream! {
            loop {
                let duration = if end_block.is_latest() {
                    match rx.recv().await {
                        Some(block) if current_block > end_block_num => {
                            end_block_num = block.header.number.unwrap();
                            Duration::from_millis(800)
                        },
                        _ => Duration::from_millis(100),
                    }
                } else if current_block > end_block_num {
                    break;
                } else {
                    Duration::from_millis(100)
                };

                sleep(duration).await;

                let ret = client.get_block_by_number(current_block.into(), false).await;

                if let Ok(Some(block)) = ret {
                    yield block;
                    current_block += 1;
                }
            }
        };

        // Return the stream of blocks
        Ok(block_stream)
    }
}

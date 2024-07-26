use super::EthereumConfig;
use alloy::{
    primitives::BlockNumber,
    providers::{Provider, ProviderBuilder, RootProvider, WsConnect},
    pubsub::PubSubFrontend,
    rpc::types::{Block, BlockTransactionsKind},
    transports::http::{Client, Http},
};
use anyhow::{anyhow, Result};
use async_stream::stream;
use futures::{Stream, StreamExt};
use std::{pin::Pin, sync::Arc, time::Duration};
use tokio::{
    sync::{watch, Mutex},
    time::{sleep, timeout},
};

pub type BlockStream = Pin<Box<dyn Stream<Item = Result<Block>> + Send>>;

// Ethereum is a struct that represents an Ethereum client.
pub struct Ethereum {
    http_provider_tx: Option<watch::Sender<RootProvider<Http<Client>>>>,
    ws_provider_tx: Option<watch::Sender<RootProvider<PubSubFrontend>>>,
    latest_block: Arc<Mutex<BlockNumber>>,
    timeout: Duration,
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

        // Create the watch channels for the http provider
        let http_provider_tx = http_provider.map(|provider| {
            let (tx, _rx) = watch::channel(provider);
            tx
        });

        // Create the watch channels for the ws provider
        let ws_provider_tx = ws_provider.map(|provider| {
            let (tx, _rx) = watch::channel(provider);
            tx
        });

        let latest_block = Arc::new(Mutex::new(BlockNumber::MIN));
        let latest_block_cloned = latest_block.clone();

        let ethereum = Ethereum {
            latest_block,
            http_provider_tx,
            ws_provider_tx,
            timeout: config.timeout,
        };

        if config.subscribe_latest_block {
            let mut block_stream = ethereum.subscribe_blocks().await?;

            tokio::spawn(async move {
                while let Some(Ok(block)) = block_stream.next().await {
                    if let Some(block_number) = block.header.number {
                        let mut latest_block_guard = latest_block_cloned.lock().await;
                        *latest_block_guard = block_number;
                    }
                }
            });
        }

        Ok(ethereum)
    }

    pub async fn update_http_provider(&self, url: &str) -> Result<()> {
        let tx = self
            .http_provider_tx
            .as_ref()
            .ok_or_else(|| anyhow!("HTTP provider not configured"))?;

        let url = url.parse()?;
        let provider = ProviderBuilder::new().on_http(url);
        tx.send(provider)?;
        Ok(())
    }

    pub async fn update_ws_provider(&self, url: &str) -> Result<()> {
        let tx = self
            .ws_provider_tx
            .as_ref()
            .ok_or_else(|| anyhow!("Websocket provider not configured"))?;

        let connect = WsConnect::new(url);
        let provider = ProviderBuilder::new().on_ws(connect).await?;
        tx.send(provider)?;
        Ok(())
    }

    pub async fn subscribe_blocks(&self) -> Result<BlockStream> {
        let mut rx = self
            .ws_provider_tx
            .as_ref()
            .ok_or_else(|| anyhow!("Websocket provider not configured"))?
            .subscribe();

        let duration = self.timeout;

        let block_stream = stream! {
            loop {
                let provider = rx.borrow_and_update().clone();
                let subscription = provider.subscribe_blocks().await?;
                let mut block_stream = subscription.into_stream();

                while let Ok(Some(block)) = timeout(duration, block_stream.next()).await {
                    if let Ok(true) = rx.has_changed() {
                        break;
                    }

                    yield Ok(block);
                }

                if rx.changed().await.is_err() {
                    break;
                }

                println!("Subscribe Blocks restart");
            }
        };

        // Return the stream of blocks
        Ok(Box::pin(block_stream))
    }

    pub async fn watch_blocks(&self) -> Result<BlockStream> {
        let mut rx = self
            .http_provider_tx
            .as_ref()
            .ok_or_else(|| anyhow!("HTTP provider not configured"))?
            .subscribe();
        let duration = self.timeout;

        let block_stream = stream! {
            loop {
                let provider = rx.borrow_and_update().clone();
                let mut block_hash_stream = provider.watch_blocks().await?.into_stream();

                'outer: while let Ok(Some(block_hashes)) = timeout(duration, block_hash_stream.next()).await {
                    if let Ok(true) = rx.has_changed() {
                        break 'outer;
                    }

                    for block_hash in block_hashes {
                        if let Ok(true) = rx.has_changed() {
                            break 'outer;
                        }

                        let ret = provider.get_block_by_hash(block_hash, BlockTransactionsKind::Full).await?;

                        if let Some(block) = ret {
                            yield Ok(block);
                        }
                    }

                }

                if rx.changed().await.is_err() {
                    break;
                }

                println!("Watch Blocks restart");
            }
        };

        // Return the stream of blocks
        Ok(Box::pin(block_stream))
    }

    pub async fn fetch_blocks(
        &self,
        start: BlockNumber,
        confirmations: BlockNumber,
    ) -> Result<BlockStream> {
        let mut rx = self
            .http_provider_tx
            .as_ref()
            .ok_or_else(|| anyhow!("HTTP provider not configured"))?
            .subscribe();
        let latest_block_cloned = self.latest_block.clone();
        let mut current_block = start;

        let duration = self.timeout;

        let block_stream = stream! {
            loop {
                let provider = rx.borrow_and_update().clone();

                loop {
                    if let Ok(true) = rx.has_changed() {
                        break;
                    }

                    let latest_block = *latest_block_cloned.lock().await;

                    if latest_block < confirmations || current_block > latest_block - confirmations {
                        sleep(Duration::from_millis(1000)).await;
                    } else {
                        let ret = timeout(duration, provider.get_block_by_number(current_block.into(), false)).await;

                        if let Ok(Ok(Some(block))) = ret {
                            yield Ok(block);
                            current_block += 1;
                        } else {
                            break;
                        }

                        sleep(Duration::from_millis(100)).await;
                    };
                }

                if rx.changed().await.is_err() {
                    break;
                }
            }
        };

        // Return the stream of blocks
        Ok(Box::pin(block_stream))
    }
}

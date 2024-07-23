use crate::utils::get_timestamp;
use alloy::{
    primitives::BlockNumber,
    providers::{Provider, ProviderBuilder, WsConnect},
    transports::TransportError,
};
use std::{
    cmp::Ordering,
    collections::{BinaryHeap, HashSet},
    fmt::Debug,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    sync::{
        mpsc::{self, error::SendError, Sender},
        Mutex,
    },
    time,
};
use tokio_stream::{
    wrappers::{IntervalStream, ReceiverStream},
    StreamExt,
};
use url::Url;

#[derive(Debug)]
pub struct RpcUrlList {
    // urls 为 rpc url 的优先级队列，优先级由 RpcUrl 的 height 和 latency 决定
    urls: Arc<Mutex<BinaryHeap<RpcUrl>>>,
    // duration 为 rpc url 的健康检查间隔时间
    duration: Duration,
    // timeout 为 rpc url 的超时时间, 超时后会更新 rpc url 的状态为 timeout
    timeout: Duration,
    // buffer_size 为 rpc url 的数量, 用于设置 mpsc channel 的缓冲区大小
    buffer_size: usize,
}

impl RpcUrlList {
    pub fn new(urls: Vec<Url>, duration: Duration, timeout: Duration) -> Self {
        let mut rpc_urls_set = HashSet::new();
        let mut rpc_urls = BinaryHeap::new();

        for url in urls {
            if matches!(url.scheme(), "http" | "https" | "ws" | "wss")
                && rpc_urls_set.insert(url.clone())
            {
                rpc_urls.push(RpcUrl::new(url));
            }
        }

        let rpc_url_list = RpcUrlList {
            urls: Arc::new(Mutex::new(rpc_urls)),
            duration,
            timeout,
            buffer_size: rpc_urls_set.len(),
        };

        rpc_url_list.health_check();

        rpc_url_list
    }

    pub async fn rpc_url(&self) -> Option<RpcUrl> {
        let scheme_matcher = |scheme: &str| matches!(scheme, "http" | "https");
        self.find_rpc_url(scheme_matcher).await
    }

    pub async fn ws_url(&self) -> Option<RpcUrl> {
        let scheme_matcher = |scheme: &str| matches!(scheme, "ws" | "wss");
        self.find_rpc_url(scheme_matcher).await
    }

    pub async fn get_url(&self, url: Url) -> Option<RpcUrl> {
        let urls = self.urls.lock().await;
        urls.iter().find(|rpc_url| rpc_url.url == url).cloned()
    }

    pub async fn all_rpc_urls(&self) -> Vec<RpcUrl> {
        let urls = self.urls.lock().await;
        urls.iter().cloned().collect()
    }

    async fn find_rpc_url<F>(&self, scheme_matcher: F) -> Option<RpcUrl>
    where
        F: Fn(&str) -> bool,
    {
        let urls = self.urls.lock().await;
        urls.iter()
            .find(|rpc_url| scheme_matcher(rpc_url.url.scheme()))
            .cloned()
    }

    // 健康检查,在新的线程中执行，为每个rpc url也创建一个新的线程，并更新rpc url的状态
    fn health_check(&self) {
        let duration = self.duration;
        let timeout = self.timeout;
        let buffer_size = self.buffer_size;
        let urls = Arc::clone(&self.urls);

        tokio::spawn(async move {
            let interval = time::interval(duration);
            let mut stream = IntervalStream::new(interval);

            while stream.next().await.is_some() {
                let (tx, rx) = mpsc::channel::<RpcUrl>(buffer_size);
                let mut urls_guard = urls.lock().await;

                for rpc_url in urls_guard.iter() {
                    check_rpc_url(rpc_url, timeout, tx.clone());
                }

                // drop tx to close the channel
                // rx will return None after tx is dropped
                // so the while loop will break
                drop(tx);

                let mut stream = ReceiverStream::new(rx);

                while let Some(rpc_url) = stream.next().await {
                    urls_guard.retain(|url| url.url != rpc_url.url);
                    urls_guard.push(rpc_url);
                }
            }
        });
    }
}

fn check_rpc_url(rpc_url: &RpcUrl, timeout: Duration, tx: Sender<RpcUrl>) {
    let rpc_url = rpc_url.to_owned().clone();
    let url = rpc_url.url.clone();

    tokio::spawn(async move {
        let task = async {
            let (height, latency) = match rpc_url.url.scheme() {
                "http" | "https" => {
                    let provider = ProviderBuilder::new().on_http(url);
                    let start = Instant::now();
                    let height = provider.get_block_number().await;
                    let latency = start.elapsed();
                    (height, latency)
                }
                "ws" | "wss" => {
                    let ws = WsConnect::new(url);
                    let provider = ProviderBuilder::new().on_ws(ws).await.map_err(|_err| {
                        SendError(RpcUrl {
                            latency: None,
                            request_time: Some(get_timestamp()),
                            request_status: Some(RequestStatus::Failure),
                            ..rpc_url.clone()
                        })
                    })?;
                    let start = Instant::now();
                    let height = provider.get_block_number().await;
                    let latency = start.elapsed();
                    (height, latency)
                }
                _ => (
                    Err(TransportError::local_usage_str("Unsupported scheme")),
                    Duration::from_secs(0),
                ),
            };

            let request_status = if height.is_ok() {
                RequestStatus::Success
            } else {
                RequestStatus::Failure
            };

            let rpc_url = RpcUrl {
                height: height.ok(),
                latency: Some(latency),
                request_time: Some(get_timestamp()),
                request_status: Some(request_status),
                ..rpc_url.clone()
            };

            tx.send(rpc_url).await?;

            Ok::<(), SendError<RpcUrl>>(())
        };

        // set timeout for task
        let ret = time::timeout(timeout, task).await;

        // if task timeout, update the RpcUrl status to timeout
        if ret.is_err() {
            let rpc_url = RpcUrl {
                latency: None,
                request_time: Some(get_timestamp()),
                request_status: Some(RequestStatus::Timeout),
                ..rpc_url
            };

            tx.send(rpc_url).await?;
        }

        Ok::<(), SendError<RpcUrl>>(())
    });
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RequestStatus {
    Success,
    Failure,
    Timeout,
    Unknown,
}

#[derive(Clone, PartialEq, Eq)]
pub struct RpcUrl {
    url: Url,
    height: Option<BlockNumber>,
    latency: Option<Duration>,
    request_time: Option<u64>,
    request_status: Option<RequestStatus>,
}

impl RpcUrl {
    fn new(url: Url) -> Self {
        RpcUrl {
            url,
            height: None,
            latency: None,
            request_time: None,
            request_status: None,
        }
    }
}

impl Debug for RpcUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RpcUrl")
            .field("url", &self.url.to_string())
            .field("height", &self.height.unwrap_or(0))
            .field("latency", &self.latency.unwrap_or(Duration::from_secs(0)))
            .field("request_time", &self.request_time.unwrap_or(0))
            .field(
                "request_status",
                &self
                    .request_status
                    .as_ref()
                    .unwrap_or(&RequestStatus::Unknown),
            )
            .finish()
    }
}

impl Ord for RpcUrl {
    fn cmp(&self, other: &Self) -> Ordering {
        // request_status 为 failure 或者 timeout 时，优先级最低
        // height 为 None 时，优先级最低
        // latency 为 None 时，优先级最低
        use RequestStatus::*;

        // 优先级最低的情况
        if matches!(self.request_status, Some(Failure) | Some(Timeout)) {
            return Ordering::Less;
        }
        if matches!(other.request_status, Some(Failure) | Some(Timeout)) {
            return Ordering::Greater;
        }

        if self.height.is_none() {
            return Ordering::Less;
        }
        if other.height.is_none() {
            return Ordering::Greater;
        }

        if self.latency.is_none() {
            return Ordering::Less;
        }
        if other.latency.is_none() {
            return Ordering::Greater;
        }

        // 按 height 排序，height 越大优先级越高
        // height 相同时，按 latency 排序，latency 越小优先级越高
        match self.height.cmp(&other.height) {
            Ordering::Equal => self.latency.cmp(&other.latency).reverse(),
            other => other,
        }
    }
}

impl PartialOrd for RpcUrl {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

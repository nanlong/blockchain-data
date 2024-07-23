use alloy::{
    primitives::BlockNumber,
    providers::{Provider, ProviderBuilder, WsConnect},
    transports::TransportError,
};
use std::{
    cmp::Ordering,
    collections::{BinaryHeap, HashSet},
    fmt::{self, Debug},
    sync::Arc,
    time::{Duration, Instant, SystemTime},
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
                    check_rpc_url(rpc_url, duration, timeout, tx.clone());
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

fn check_rpc_url(rpc_url: &RpcUrl, duration: Duration, timeout: Duration, tx: Sender<RpcUrl>) {
    let mut rpc_url = rpc_url.to_owned().clone();
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
                        let status = RequestStatus::Failure;
                        rpc_url.uptime.offline(duration);
                        rpc_url.success_rate.update(&status);

                        let new_rpc_url = RpcUrl {
                            request_time: Some(SystemTime::now()),
                            request_status: Some(status),
                            ..rpc_url.clone()
                        };

                        SendError(new_rpc_url)
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

            rpc_url.latency.update(latency);

            let status = if height.is_ok() {
                rpc_url.uptime.online(duration);
                RequestStatus::Success
            } else {
                rpc_url.uptime.offline(duration);
                RequestStatus::Failure
            };
            rpc_url.success_rate.update(&status);

            let rpc_url = RpcUrl {
                height: height.ok(),
                request_time: Some(SystemTime::now()),
                request_status: Some(status),
                ..rpc_url.clone()
            };

            tx.send(rpc_url).await?;

            Ok::<(), SendError<RpcUrl>>(())
        };

        // set timeout for task
        let ret = time::timeout(timeout, task).await;

        match ret {
            // if task timeout, update the RpcUrl status to timeout
            Err(_) => {
                let status = RequestStatus::Timeout;
                rpc_url.uptime.offline(timeout);
                rpc_url.latency.update(timeout);
                rpc_url.success_rate.update(&status);

                let rpc_url = RpcUrl {
                    request_time: Some(SystemTime::now()),
                    request_status: Some(status),
                    ..rpc_url
                };

                tx.send(rpc_url).await?;
            }
            Ok(Err(SendError(rpc_url))) => {
                tx.send(rpc_url).await?;
            }
            _ => {}
        }

        Ok::<(), SendError<RpcUrl>>(())
    });
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RequestStatus {
    Success,
    Failure,
    Timeout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Latency {
    total_count: u32,
    max_latency: Duration,
    min_latency: Duration,
    avg_latency: Duration,
    latest_latency: Duration,
}

impl Latency {
    fn new() -> Self {
        Latency {
            total_count: 0,
            max_latency: Duration::from_secs(0),
            min_latency: Duration::from_secs(u64::MAX),
            avg_latency: Duration::from_secs(0),
            latest_latency: Duration::from_secs(0),
        }
    }

    fn update(&mut self, latency: Duration) {
        self.total_count += 1;
        self.max_latency = self.max_latency.max(latency);
        self.min_latency = self.min_latency.min(latency);
        self.avg_latency = (self.avg_latency * (self.total_count - 1) + latency) / self.total_count;
        self.latest_latency = latency;
    }

    fn score(&self) -> u8 {
        match self.avg_latency.as_millis() {
            0..=100 => 10,
            101..=200 => 9,
            201..=500 => 8,
            501..=1000 => 7,
            _ => 6,
        }
    }
}

impl fmt::Display for Latency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let latency = self.latest_latency.as_millis() as f64 / 1000.0;
        write!(f, "{:?}", latency.to_string() + "s")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Uptime {
    start_time: Instant,
    total_time: Duration,
    online_time: Duration,
}

impl Uptime {
    fn new() -> Self {
        Uptime {
            start_time: Instant::now(),
            total_time: Duration::from_secs(0),
            online_time: Duration::from_secs(0),
        }
    }

    fn online(&mut self, duration: Duration) {
        self.total_time += duration;
        self.online_time += duration;
    }

    fn offline(&mut self, duration: Duration) {
        self.total_time += duration;
    }

    fn percentage(&self) -> f64 {
        if self.total_time.as_secs() == 0 {
            0.0
        } else {
            self.online_time.as_secs_f64() / self.total_time.as_secs_f64() * 100.0
        }
    }

    fn score(&self) -> u8 {
        match self.percentage() {
            99.9..=100.0 => 10,
            99.5..=99.9 => 9,
            99.0..=99.5 => 8,
            _ => 7,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SuccessRate {
    total_count: u32,
    success_count: u32,
}

impl SuccessRate {
    fn new() -> Self {
        SuccessRate {
            total_count: 0,
            success_count: 0,
        }
    }

    fn update(&mut self, status: &RequestStatus) {
        self.total_count += 1;
        if status == &RequestStatus::Success {
            self.success_count += 1;
        }
    }

    fn percentage(&self) -> f64 {
        if self.total_count == 0 {
            0.0
        } else {
            self.success_count as f64 / self.total_count as f64 * 100.0
        }
    }

    fn score(&self) -> u8 {
        match self.percentage() {
            99.9..=100.0 => 10,
            99.5..=99.9 => 9,
            99.0..=99.5 => 8,
            _ => 7,
        }
    }
}

// 请求成功率（Success Rate）
#[derive(Clone, PartialEq, Eq)]
pub struct RpcUrl {
    url: Url,
    height: Option<BlockNumber>,
    latency: Latency,
    uptime: Uptime,
    success_rate: SuccessRate,
    request_time: Option<SystemTime>,
    request_status: Option<RequestStatus>,
    score: u64,
}

impl RpcUrl {
    fn new(url: Url) -> Self {
        RpcUrl {
            url,
            height: None,
            latency: Latency::new(),
            uptime: Uptime::new(),
            success_rate: SuccessRate::new(),
            request_time: None,
            request_status: None,
            score: 0,
        }
    }

    fn score(&self) -> u64 {
        (4 * self.uptime.score() + 3 * self.latency.score() + 3 * self.success_rate.score()) as u64
    }
}

impl Debug for RpcUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RpcUrl")
            .field("url", &self.url.to_string())
            .field("height", &self.height)
            .field("latency", &self.latency.avg_latency.as_secs_f32())
            .field("uptime", &self.uptime.percentage())
            .field("success_rate", &self.success_rate.percentage())
            .field("request_time", &self.request_time)
            .field("request_status", &self.request_status)
            .field("score", &self.score())
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

        // 按 height 排序，height 越大优先级越高
        // height 相同时，按 score score 越大优先级越高
        match self.height.cmp(&other.height) {
            Ordering::Equal => self.score().cmp(&other.score()),
            ordering => ordering,
        }
    }
}

impl PartialOrd for RpcUrl {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

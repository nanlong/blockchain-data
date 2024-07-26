use std::time::Duration;

use derive_builder::Builder;
use url::Url;

#[derive(Builder)]
pub struct EthereumConfig {
    #[builder(default, setter(strip_option))]
    pub(crate) rpc_url: Option<Url>,
    #[builder(default, setter(strip_option))]
    pub(crate) ws_url: Option<Url>,
    #[builder(default = "true")]
    pub(crate) subscribe_latest_block: bool,
    #[builder(default = "Duration::from_secs(1)")]
    pub(crate) timeout: Duration,
}

impl EthereumConfig {
    pub fn builder() -> EthereumConfigBuilder {
        EthereumConfigBuilder::default()
    }
}

use derive_builder::Builder;
use url::Url;

#[derive(Builder)]
pub struct EthereumConfig {
    #[builder(default, setter(strip_option))]
    pub(crate) rpc_url: Option<Url>,
    #[builder(default, setter(strip_option))]
    pub(crate) ws_url: Option<Url>,
}

impl EthereumConfig {
    pub fn builder() -> EthereumConfigBuilder {
        EthereumConfigBuilder::default()
    }
}

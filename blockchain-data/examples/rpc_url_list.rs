use blockchain_data::EthereumRpcUrlList;
use std::time::Duration;
use tokio_stream::{wrappers::IntervalStream, StreamExt};

#[tokio::main()]
async fn main() -> anyhow::Result<()> {
    let rpc_url_list = EthereumRpcUrlList::new(
        vec![
            "https://api.securerpc.com/v1".parse()?,
            "wss://ethereum-rpc.publicnode.com".parse()?,
            "wss://eth.drpc.org".parse()?,
        ],
        Duration::from_secs(3),
        Duration::from_secs(3),
    );

    let interval = tokio::time::interval(Duration::from_secs(2));
    let mut stream = IntervalStream::new(interval);

    while (stream.next().await).is_some() {
        println!("rpc urls: {:#?}", rpc_url_list.all_rpc_urls().await);
        // println!("rpc url: {:#?}", rpc_url_list.rpc_url().await);
        // println!("ws url: {:#?}", rpc_url_list.ws_url().await);
    }

    Ok(())
}

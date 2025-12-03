use raito_bitcoin_client::ZcashClient;
use zcash_primitives::block::BlockHash;

#[tokio::main]
async fn main() {
    let client = ZcashClient::new(
        "https://go.getblock.io/5c5842f906c341c5a50cf95b602d0a09".to_string(),
        None
    )
    .await
    .unwrap();

    let hash = client.get_block_hash(3156073)
    .await
    .unwrap();

    println!("hash: {}", hash);
    let header = client.get_block_header(&hash).await.unwrap();

    println!("got header! hash: {}", header.hash());

    let height = client.get_block_height(&hash).await.unwrap();

    println!("height: {}", height);


    let header_by_height = client.get_block_header_by_height(height).await.unwrap();

    println!("header_by_height: {}", header_by_height.0.hash());

    let chain_height = client.get_chain_height().await.unwrap();

    println!("chain_height: {}", chain_height);
}
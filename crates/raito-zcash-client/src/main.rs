use raito_zcash_client::ZcashClient;

#[tokio::main]
async fn main() {
    let client = ZcashClient::new(
        "https://go.getblock.io/5c5842f906c341c5a50cf95b602d0a09".to_string(),
        None,
    )
    .await
    .unwrap();

    let tx_bytes =
        hex::decode("43e966e190f8a63dda0add470e9439b2163f3c89a857488b966d6af2ee716851").unwrap();

    let tx = client.get_transaction(tx_bytes.as_slice()).await.unwrap();

    println!("tx: {tx:?}");

    let hash = tx.txid();
    println!("hash: {hash}");

    // let hash = client.get_block_hash(3156073).await.unwrap();

    // println!("hash: {hash}");
    // let header = client.get_block_header(&hash).await.unwrap();

    // println!("got header! hash: {}", header.hash());

    // let height = client.get_block_height(&hash).await.unwrap();

    // println!("height: {height}");

    // let header_by_height = client.get_block_header_by_height(height).await.unwrap();

    // println!("header_by_height: {}", header_by_height.0.hash());

    // let chain_height = client.get_chain_height().await.unwrap();

    // println!("chain_height: {chain_height}");
}

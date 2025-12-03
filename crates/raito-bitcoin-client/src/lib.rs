//! Zcash RPC client for fetching block headers, transactions and chain information with retry logic.

use base64::{engine::general_purpose, Engine as _};
use jsonrpsee::core::client::ClientT;
use jsonrpsee::core::params::ArrayParams;
use jsonrpsee::http_client::{HeaderMap, HeaderValue, HttpClient};
use jsonrpsee::rpc_params;
use serde::de::DeserializeOwned;
use serde_json::Value;
use zcash_primitives::block::{BlockHash, BlockHeader};
use zcash_primitives::transaction::Transaction;
use zcash_protocol::{
    consensus::{BlockHeight, BranchId, Network},
    TxId,
};
use std::time::Duration;
use thiserror::Error;
use tracing::{debug, info};

/// Error types for Bitcoin RPC client operations
#[derive(Error, Debug)]
pub enum ZcashClientError {
    /// RPC client errors
    #[error("RPC client error: {0}")]
    RpcClient(#[from] jsonrpsee::core::client::Error),
    /// Invalid HTTP header value
    #[error("Invalid HTTP header value")]
    InvalidHeader,
    /// Failed to decode hex response
    #[error("Failed to decode hex response: {0}")]
    HexDecode(#[from] hex::FromHexError),
    /// Failed to read Zcash block header
    #[error("Failed to read Zcash block header: {0}")]
    ZcashBlockHeaderRead(#[from] std::io::Error),
    /// Failed to read Zcash transaction
    #[error("Failed to read Zcash transaction: {0}")]
    ZcashTransactionRead(std::io::Error),
    /// Unsupported or unknown network reported by node
    #[error("Unsupported Zcash network: {0}")]
    UnsupportedNetwork(String),
    /// Failed to convert block hash
    #[error("Failed to convert block hash: {0}")]
    InvalidBlockHash(String),
}

/// Default HTTP request timeout
pub const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Default chain height update interval in seconds
pub const CHAIN_HEIGHT_UPDATE_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub struct ZcashClient {
    client: HttpClient,
    chain_height: u32,
    backoff: backoff::ExponentialBackoff,
    network: Network,
}

impl ZcashClient {
    /// Create a new Zcash RPC client with default retry settings (exponential backoff)
    pub async fn new(url: String, userpwd: Option<String>) -> Result<Self, ZcashClientError> {
        let mut headers = HeaderMap::new();
        if let Some(userpwd) = userpwd {
            let creds = general_purpose::STANDARD.encode(userpwd);
            headers.insert(
                "Authorization",
                HeaderValue::from_str(&format!("Basic {creds}"))
                    .map_err(|_| ZcashClientError::InvalidHeader)?,
            );
        };

        let client = HttpClient::builder()
            .set_headers(headers)
            .request_timeout(HTTP_REQUEST_TIMEOUT)
            .build(url)?;

        let backoff = backoff::ExponentialBackoff::default();
        
        let chain_info = Self::get_chain_info(client.clone(), backoff.clone()).await?;
        let network = match chain_info["chain"].as_str().unwrap_or("unknown") {
            "main" => Network::MainNetwork,
            "test" => Network::TestNetwork,
            _ => return Err(ZcashClientError::UnsupportedNetwork("unknown network".to_string())),
        };
        
        Ok(Self {
            client,
            backoff: backoff.clone(),
            network,
            chain_height: chain_info["blocks"].as_u64().unwrap_or(0) as u32,
        })
    }

    /// Get chain info, needed to determine the network
    async fn get_chain_info(client: HttpClient, backoff: backoff::ExponentialBackoff) -> Result<Value, ZcashClientError> {
        let blockchain_info: Value =
            request_with_retry(backoff, || async {
                client
                    .request("getblockchaininfo", rpc_params![])
                    .await
                    .map_err(Into::into)
            })
            .await?;
        Ok(blockchain_info)
    }

    async fn request<T: DeserializeOwned>(
        &self,
        method: &str,
        params: ArrayParams,
    ) -> Result<T, ZcashClientError> {
        request_with_retry(self.backoff.clone(), || async {
            self.client
                .request(method, params.clone())
                .await
                .map_err(Into::into)
        })
        .await
    }

    /// Get block hash by height
    pub async fn get_block_hash(&self, height: u32) -> Result<BlockHash, ZcashClientError> {
        self.request::<String>("getblockhash", rpc_params![height])
            .await
            .and_then(|s| {
                let mut bytes = hex::decode(&s)?;
                bytes.reverse(); // we need to reverse endianness to match rpc
                BlockHash::try_from_slice(&bytes)
                    .ok_or_else(|| ZcashClientError::InvalidBlockHash(s))
            })
    }

    /// Get block header by hash
    pub async fn get_block_header(
        &self,
        hash: &BlockHash,
    ) -> Result<BlockHeader, ZcashClientError> {
        self.request::<String>("getblockheader", rpc_params![hash.to_string(), false])
            .await
            .and_then(|header_hex| {
                let header_bytes = hex::decode(header_hex)?;
                let mut reader = header_bytes.as_slice();
                BlockHeader::read(&mut reader).map_err(Into::into)
            })
    }

    /// Get block height by hash
    pub async fn get_block_height(&self, hash: &BlockHash) -> Result<u32, ZcashClientError> {
        let header_info: serde_json::Value = self
            .request("getblockheader", rpc_params![hash.to_string(), true])
            .await?;
        let block_height = decode_block_height(&header_info)?;
        Ok(block_height)
    }

    /// Get block header by height
    pub async fn get_block_header_by_height(
        &self,
        height: u32,
    ) -> Result<(BlockHeader, BlockHash), ZcashClientError> {
        let hash = self.get_block_hash(height).await?;
        let header = self.get_block_header(&hash).await?;
        Ok((header, hash))
    }

    /// Get transaction by txid and hash of the block containing the transaction
    pub async fn get_transaction(
        &self,
        txid: &TxId,
    ) -> Result<Transaction, ZcashClientError> {
        unimplemented!();
        // let header_info: serde_json::Value = self
        //     .request("getblockheader", rpc_params![block_hash, true])
        //     .await?;

        // let block_height = decode_block_height(&header_info)?;

        // let consensus_branch_id =
        //     BranchId::for_height(&self.network, BlockHeight::from(block_height));

        // let raw_transaction_bytes = hex::decode(raw_transaction)?;
        // let mut reader = raw_transaction_bytes.as_slice();
        // let transaction =
        //     Transaction::read(&mut reader, consensus_branch_id)
        //         .map_err(ZcashClientError::ZcashTransactionRead)?;
        // Ok(transaction)
    }

    /// Get transaction inclusion proof
    pub async fn get_transaction_inclusion_proof(
        &self,
        _txid: &TxId,
    ) -> Result<(), ZcashClientError> {
        unimplemented!();
        // self.request("gettxoutproof", rpc_params![[txid.to_string()]])
        //     .await
    }

    /// Get current chain height
    pub async fn get_chain_height(&self) -> Result<u32, ZcashClientError> {
        let result: u64 = self.request("getblockcount", rpc_params![]).await?;
        Ok(result as u32)
    }

    /// Wait for a block header at the given height.
    /// If the specified lag is non-zero, the function will wait till `lag` blocks are built on top of the expected block.
    pub async fn wait_block_header(
        &mut self,
        height: u32,
        lag: u32,
    ) -> Result<(BlockHeader, BlockHash), ZcashClientError> {
        while height > self.chain_height {
            self.chain_height = self.get_chain_height().await?.saturating_sub(lag);
            if height <= self.chain_height {
                debug!("New chain height: {}", self.chain_height);
                break;
            } else {
                tokio::time::sleep(CHAIN_HEIGHT_UPDATE_INTERVAL).await;
            }
        }
        self.get_block_header_by_height(height).await
    }
}

fn decode_block_height(header_info: &serde_json::Value) -> Result<u32, ZcashClientError> {
    header_info.get("height")
        .and_then(|h| h.as_u64())
        .map(|h| h as u32)
        .ok_or_else(|| ZcashClientError::ZcashBlockHeaderRead(
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "missing or invalid block height in getblockheader response",
            )
        ))
}

/// Execute a request with retry logic using exponential backoff
/// Only retries on unexpected HTTP errors (not 200 OK or 400 Bad Request)
async fn request_with_retry<F, Fut, T>(
    backoff: backoff::ExponentialBackoff,
    operation: F,
) -> Result<T, ZcashClientError>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T, ZcashClientError>>,
{
    use backoff::{future::retry_notify, Error};

    retry_notify(
        backoff,
        || async {
            match operation().await {
                Ok(result) => Ok(result),
                Err(err) => {
                    // Check if this is a retryable HTTP error
                    if is_retryable_error(&err) {
                        Err(Error::transient(err))
                    } else {
                        Err(Error::permanent(err))
                    }
                }
            }
        },
        |err, duration| {
            info!("Request failed, retrying in {:?}: {}", duration, err);
        },
    )
    .await
}

/// Determines if an error should be retried - only retry HTTP errors (except bad request)
fn is_retryable_error(err: &ZcashClientError) -> bool {
    match err {
        // Only retry RPC client errors that are HTTP-related (transport, timeouts, server errors)
        ZcashClientError::RpcClient(rpc_err) => {
            use jsonrpsee::core::client::Error as RpcError;
            match rpc_err {
                // Only retry transport errors and timeouts (HTTP-level issues)
                RpcError::Transport(_) => true,
                RpcError::RequestTimeout => true,
                RpcError::RestartNeeded(_) => true,
                RpcError::ServiceDisconnect => true,
                // Don't retry any other RPC errors (JSON-RPC level issues, bad requests, etc.)
                _ => false,
            }
        }
        // Don't retry any other error types (hex decode, bitcoin deserialization, header issues)
        _ => false,
    }
}


mod tests {
    use super::*;

    #[tokio::test]
    async fn test_get_transactions() {
        let client = ZcashClient::new(
            "https://go.getblock.io/ac2f0972c36143af84a242ea1e740dfa".to_string(),
            None
        )
        .await
        .unwrap();

        let txids_hex = [
            "55ab20ae2d528fd612ed55b419950ebac20f4c59ea841b7fe5db97f9c3e7e206",
            "a6cabf193af5066654d9929e54ea6bc1f794c5d07d7247a3893eeef4e5bfe17f",
            "b2aa4c149a451d75fff16d0b97291dab06cb2788ebc44be3cfeb61b847446c2b",
            "c61e5ce69c9892ee36602d6d31458f381750d52983fc471f874f95f57d9afeab",
            "84832e66f3261737b84da806f62bc07dce03e3002bef412766faa3d123f066e1",
            "ac694dd10970909bf1bfc6bd71f5e6c924b174a5ddf6529f5ba3b8e721724f9c",
            "381b65eb3fa04c1c78e73d4488b7e0b02f0469e5bd8e222f84c7896410e966dd",
            "0a5803ee986c48fb9b8c8d949ee6b4e8f48c2d81a94fb36bbf168d66753a0d41",
        ];

        for txid_hex in txids_hex {
            // Remove optional "0x" and parse hex string to bytes
            let cleaned = txid_hex.trim_start_matches("0x");
            let txid_bytes = hex::decode(cleaned).expect("Invalid txid hex");
            let mut txid_arr = [0u8; 32];
            txid_arr.copy_from_slice(&txid_bytes);
            let txid = TxId::from_bytes(txid_arr);

            // Placeholder block hash (use real block hashes if available)
            let block_hash = zcash_primitives::block::BlockHash([0u8; 32]);

            let transaction = client.get_transaction(&txid, &block_hash).await;
            match transaction {
                Ok(tx) => println!("{:?}", tx),
                Err(e) => println!("Failed for txid {}: {:?}", txid_hex, e),
            }
        }
    }
}
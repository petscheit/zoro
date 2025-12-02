//! Bitcoin RPC client for fetching block headers and chain information with retry logic.

use base64::{engine::general_purpose, Engine as _};
use bitcoin::block::Header as BlockHeader;
use bitcoin::consensus::Decodable;
use bitcoin::MerkleBlock;
use bitcoin::{BlockHash, Transaction, Txid};
use bitcoincore_rpc_json::GetBlockHeaderResult;
use jsonrpsee::core::client::ClientT;
use jsonrpsee::core::params::ArrayParams;
use jsonrpsee::http_client::{HeaderMap, HeaderValue, HttpClient};
use jsonrpsee::rpc_params;
use serde::de::DeserializeOwned;
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
    /// Failed to deserialize Bitcoin consensus data
    #[error("Failed to deserialize Bitcoin data: {0}")]
    BitcoinDeserialization(#[from] bitcoin::consensus::encode::Error),
}

/// Default HTTP request timeout
pub const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Default chain height update interval in seconds
pub const CHAIN_HEIGHT_UPDATE_INTERVAL: Duration = Duration::from_secs(10);

/// Bitcoin RPC client
#[derive(Debug)]
pub struct ZcashClient {
    client: HttpClient,
    chain_height: u32,
    backoff: backoff::ExponentialBackoff,
}

impl ZcashClient {
    /// Create a new Zcash RPC client with default retry settings (exponential backoff)
    pub fn new(url: String, userpwd: Option<String>) -> Result<Self, ZcashClientError> {
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

        Ok(Self {
            client,
            chain_height: 0,
            backoff: backoff::ExponentialBackoff::default(),
        })
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
        self.request("getblockhash", rpc_params![height]).await
    }

    /// Get block header by hash
    pub async fn get_block_header(
        &self,
        hash: &BlockHash,
    ) -> Result<BlockHeader, ZcashClientError> {
        self.request::<BlockHeader>("getblockheader", rpc_params![hash.to_string(), false])
            .await
    }

    /// Get block header by hash with extended data
    pub async fn get_block_header_ex(
        &self,
        hash: &BlockHash,
    ) -> Result<GetBlockHeaderResult, ZcashClientError> {
        self.request("getblockheader", rpc_params![hash.to_string(), true])
            .await
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
        txid: &Txid,
        block_hash: &BlockHash,
    ) -> Result<Transaction, ZcashClientError> {
        unimplemented!();
        // self.request(
        //     "getrawtransaction",
        //     rpc_params![txid.to_string(), false, block_hash.to_string()],
        // )
        // .await
    }

    /// Get transaction inclusion proof
    pub async fn get_transaction_inclusion_proof(
        &self,
        txid: &Txid,
    ) -> Result<MerkleBlock, ZcashClientError> {
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

//! Zcash RPC client for fetching block headers, transactions and chain information with retry logic.

use base64::{engine::general_purpose, Engine as _};
use jsonrpsee::core::client::ClientT;
use jsonrpsee::core::params::ArrayParams;
use jsonrpsee::http_client::{HeaderMap, HeaderValue, HttpClient};
use jsonrpsee::rpc_params;
use serde::{Deserialize, Serialize};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::time::Duration;
use thiserror::Error;
use tracing::{debug, info};
use zcash_primitives::block::{BlockHash, BlockHeader};
use zcash_primitives::transaction::Transaction as ZcashTransaction;
use zcash_protocol::consensus::{BlockHeight, BranchId};
use zcash_protocol::{consensus::Network, TxId};
pub mod types;


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
            _ => {
                return Err(ZcashClientError::UnsupportedNetwork(
                    "unknown network".to_string(),
                ))
            }
        };

        Ok(Self {
            client,
            backoff: backoff.clone(),
            network,
            chain_height: chain_info["blocks"].as_u64().unwrap_or(0) as u32,
        })
    }

    /// Get chain info, needed to determine the network
    async fn get_chain_info(
        client: HttpClient,
        backoff: backoff::ExponentialBackoff,
    ) -> Result<Value, ZcashClientError> {
        let blockchain_info: Value = request_with_retry(backoff, || async {
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
    pub async fn get_transaction(&self, txid: &[u8]) -> Result<Transaction, ZcashClientError> {
        // we need to pass the txid in little endian format to the rpc
        let mut txid_le = [0u8; 32];
        txid_le.copy_from_slice(txid);
        txid_le.reverse();
        let txid = TxId::read(&mut txid_le.as_slice()).unwrap();

        // get raw tx from rpc in json mode
        let tx: Value = self
            .request("getrawtransaction", rpc_params![txid.to_string(), 1])
            .await?;

        // derive branch if from network + block height to decode tx version correctly
        let block_number: u32 = tx["height"].as_u64().unwrap() as u32;
        let consensus_branch_id =
            BranchId::for_height(&self.network, BlockHeight::from(block_number));

        // decode tx from hex
        let tx_hex = hex::decode(tx["hex"].as_str().unwrap()).unwrap();
        let transaction = Transaction::read(&mut tx_hex.as_slice(), consensus_branch_id).unwrap();

        Ok(transaction)
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
    header_info
        .get("height")
        .and_then(|h| h.as_u64())
        .map(|h| h as u32)
        .ok_or_else(|| {
            ZcashClientError::ZcashBlockHeaderRead(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "missing or invalid block height in getblockheader response",
            ))
        })
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

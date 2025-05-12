use alloy_primitives::{Address, BlockNumber, B256};
use alloy_providers::provider::HttpProvider;
use alloy_providers::provider::TempProvider;
use alloy_rpc_trace_types::geth::{
    GethDebugBuiltInTracerType, GethDebugTracerType, GethDebugTracingOptions, GethTrace,
    PreStateFrame, PreStateMode,
};
use alloy_rpc_types::{
    request::TransactionRequest, state::StateOverride, AccessListWithGasUsed, Block, BlockId,
    BlockNumberOrTag, EIP1186AccountProofResponse, FeeHistory, Filter, Log, SyncStatus,
    Transaction, TransactionReceipt,
};
use alloy_transport_http::Http;
use anyhow::{anyhow, Result};
use ethers_core::types::H256;
use hex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use url::Url;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrestateAccount {
    pub nonce: String,
    pub balance: String,
    pub code: Option<String>,
    pub storage: Option<HashMap<String, String>>,
}

pub struct Fetcher {
    provider: HttpProvider,
    rpc_url: String,
}

impl Fetcher {
    pub fn new(rpc_url: &str) -> Result<Self> {
        let http = Http::new(Url::parse(rpc_url).expect("invalid rpc url"));
        let provider: HttpProvider = HttpProvider::new(http);

        Ok(Self {
            provider,
            rpc_url: rpc_url.to_string(),
        })
    }

    pub async fn get_block_number(&self) -> Result<u64> {
        let block_num = self.provider.get_block_number().await?;
        Ok(block_num)
    }

    pub async fn get_block_by_number(&self, blk_num: u64, full: bool) -> Result<Block> {
        let block = self
            .provider
            .get_block_by_number(blk_num.into(), full)
            .await?
            .ok_or_else(|| anyhow!("Block not found"))?;
        Ok(block)
    }

    pub async fn trace_transaction(
        &self,
        tx_hash: B256,
    ) -> Result<HashMap<String, PrestateAccount>> {
        let options = GethDebugTracingOptions::default().with_tracer(
            GethDebugTracerType::BuiltInTracer(GethDebugBuiltInTracerType::PreStateTracer),
        );

        let trace = self
            .provider
            .debug_trace_transaction(tx_hash, options)
            .await?;

        let GethTrace::PreStateTracer(PreStateFrame::Default(PreStateMode(prestate))) = trace
        else {
            return Err(anyhow!(
                "Unexpected trace format: expected PreStateTracer::Default"
            ));
        };

        let mut map: HashMap<String, PrestateAccount> = HashMap::new();
        for (addr, state) in prestate {
            map.insert(
                format!("{:?}", addr), // 可选: addr.to_string() 也可以
                PrestateAccount {
                    nonce: format!("0x{:x}", state.nonce.unwrap_or(0)),
                    balance: format!("0x{:x}", state.balance.unwrap_or_default()),
                    code: state.code.as_ref().map(|c| format!("0x{}", hex::encode(c))),
                    storage: if state.storage.is_empty() {
                        None
                    } else {
                        Some(
                            state
                                .storage
                                .iter()
                                .map(|(k, v)| (format!("0x{:x}", k), format!("0x{:x}", v)))
                                .collect(),
                        )
                    },
                },
            );
        }

        Ok(map)
    }

    pub async fn trace_block(
        &self,
        blk_num: u64,
    ) -> Result<HashMap<String, HashMap<String, PrestateAccount>>> {
        let block = self.get_block_by_number(blk_num, false).await?;
        let mut result = HashMap::new();

        let hashes = match block.transactions {
            alloy_rpc_types::BlockTransactions::Hashes(ref hashes) => hashes,
            _ => return Err(anyhow!("Expected block.transactions to be hashes only")),
        };

        for hash in hashes {
            // println!("{}", format!("{:#x}", hash));
            let prestate = self.trace_transaction(*hash).await?;
            result.insert(format!("{:#x}", hash), prestate);
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    // #[tokio::test]
    // async fn test_get_block_number() {
    //     let rpc_url = "http://192.168.3.26:8545"; // 替换成你自己的 RPC URL
    //     let fetcher = super::Fetcher::new(rpc_url).expect("Failed to create fetcher");

    //     let block_number = fetcher
    //         .get_block_number()
    //         .await
    //         .expect("Failed to get block number");

    //     println!("Current block number: {}", block_number);
    // }

    // #[tokio::test]
    // async fn test_get_block() {
    //     let rpc_url = "http://192.168.3.26:8545"; // 替换成你自己的 RPC URL
    //     let fetcher = super::Fetcher::new(rpc_url).expect("Failed to create fetcher");

    //     let block_number = fetcher
    //         .get_block_number()
    //         .await
    //         .expect("Failed to get block number");
    //     println!("Current block number: {}", block_number);

    //     let block = fetcher
    //         .get_block_by_number(block_number - 2, true)
    //         .await
    //         .expect("Fail to get block");
    //     println!("block: {:#?}", block);
    // }

    #[tokio::test]
    async fn test_trace_block() {
        let rpc_url = "http://192.168.3.26:8545"; // 替换成你自己的 RPC URL
        let fetcher = super::Fetcher::new(rpc_url).expect("Failed to create fetcher");

        let block_number = fetcher
            .get_block_number()
            .await
            .expect("Failed to get block number");
        println!("Current block number: {}", block_number);

        let block = fetcher
            .trace_block(block_number - 2)
            .await
            .expect("Fail to get block");

        for (tx_hash, prestate) in block {
            println!("tx_hash: {}", tx_hash);
            println!("prestate: {:#?}", prestate);
        }
    }
}

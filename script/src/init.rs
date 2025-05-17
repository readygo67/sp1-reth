use crate::db::RemoteDb;
use crate::fetcher::{Fetcher, PrestateAccount};
use crate::SP1RethArgs;
use alloy_providers::provider::HttpProvider;
use alloy_providers::provider::TempProvider;
use alloy_rpc_types::BlockTransactions;
use alloy_transport_http::Http;
use anyhow::Result;
use async_trait::async_trait;
use reth_primitives::keccak256;
use reth_primitives::revm_primitives::{Account, AccountInfo, HashMap, B256};
use reth_primitives::Bytes;
use reth_primitives::{Address, Bytecode, U256};
use revm::db::{DbAccount, InMemoryDB};
use sp1_reth_primitives::alloy2reth::IntoReth;
use sp1_reth_primitives::mpt::proofs_to_tries;
use sp1_reth_primitives::processor::EvmProcessor;
use sp1_reth_primitives::SP1RethInput;
use std::collections::HashSet;
use std::str::FromStr;
use url::Url;

#[async_trait]
pub trait SP1RethInputInitializer {
    /// Initialize [SP1RethInput] from [SP1RethArgs].
    async fn initialize(args: &SP1RethArgs) -> Result<Self>
    where
        Self: Sized;
}

#[async_trait]
impl SP1RethInputInitializer for SP1RethInput {
    async fn initialize(args: &SP1RethArgs) -> Result<Self> {
        // Initialize the provider.
        let http = Http::new(Url::parse(&args.rpc_url).expect("invalid rpc url"));
        let provider: HttpProvider = HttpProvider::new(http);

        // Get the block.
        let parent_block = provider
            .get_block_by_number((args.block_number - 1).into(), false)
            .await?;
        let parent_header = parent_block.unwrap().header;
        let block = provider
            .get_block_by_number(args.block_number.into(), true)
            .await?
            .unwrap();

        // println!("block transactions: {:#?}", block.transactions);
        // Intiialize the db.
        let mut provider_db = RemoteDb::new(provider, parent_header.number.unwrap().as_limbs()[0]);

        // Create the input.
        let txs = match block.transactions {
            BlockTransactions::Full(txs) => txs.into_iter().map(|tx| tx.into_reth()).collect(),
            _ => unreachable!(),
        };

        let fetcher = Fetcher::new(&args.rpc_url)?;
        let prestates = fetcher.trace_block(args.block_number).await?;
        println!("success get prestates");
        //TODO 用prestates 中的值初始化 provider_db中的init_db
        let initial_db = &mut provider_db.initial_db;

        for (_tx_hash, accounts) in prestates {
            for (addr_str, account) in accounts {
                let addr = addr_str.parse::<Address>().unwrap_or_else(|_| {
                    panic!("Invalid address string from trace: {}", addr_str);
                });

                // update balance and nonce
                let mut account_info = AccountInfo::default();
                account_info.balance = U256::from_str(&account.balance).unwrap();
                account_info.nonce = u64::from_str(&account.nonce).unwrap();

                // update code
                if let Some(code_str) = &account.code {
                    if let Ok(code_bytes) = hex::decode(code_str.trim_start_matches("0x")) {
                        let bytecode = Bytecode::new_raw(code_bytes.clone().into());
                        let code_hash = keccak256(&code_bytes); // 需要 hash 模块支持

                        // 保存到 contracts 中
                        initial_db.contracts.insert(code_hash, bytecode.0.clone());
                        account_info.code_hash = code_hash;
                        account_info.code = Some(bytecode.0);
                    }
                }

                let mut db_account = DbAccount {
                    info: account_info,
                    account_state: Default::default(),
                    storage: HashMap::new(),
                };

                // 加入存储
                if let Some(storage) = &account.storage {
                    for (k, v) in storage {
                        let key = U256::from_str_radix(k.trim_start_matches("0x"), 16).unwrap();
                        let value = U256::from_str_radix(v.trim_start_matches("0x"), 16).unwrap();
                        db_account.storage.insert(key, value);
                    }
                }
                // 写入 accounts 映射
                initial_db.accounts.insert(addr, db_account);
            }
        }

        let withdrawals = block
            .withdrawals
            .unwrap()
            .into_iter()
            .map(|w| w.into_reth())
            .collect();

        let input = SP1RethInput {
            beneficiary: block.header.miner,
            gas_limit: block.header.gas_limit.try_into().unwrap(),
            timestamp: block.header.timestamp.try_into().unwrap(),
            extra_data: block.header.extra_data,
            mix_hash: block.header.mix_hash.unwrap(),
            transactions: txs,
            withdrawals,
            parent_state_trie: Default::default(),
            parent_storage: Default::default(),
            contracts: Default::default(),
            parent_header: parent_header.into_reth(),
            ancestor_headers: Default::default(),
        };

        let mut executor = EvmProcessor::<RemoteDb> {
            input: input.clone(),
            db: Some(provider_db),
            header: None,
        };
        executor.initialize();
        let mut executor = tokio::task::spawn_blocking(move || {
            executor.execute();
            executor
        })
        .await?;

        // Get the proofs and ancestor headers.
        let mut provider_db = executor.db.take().unwrap();
        let (parent_proofs, proofs, ancestor_headers, provider_db) =
            tokio::task::spawn_blocking(move || {
                let parent_proofs = provider_db.fetch_initial_storage_proofs().unwrap();
                let proofs = provider_db.fetch_latest_storage_proofs().unwrap();
                let ancestor_headers = provider_db.fetch_ancestor_headers().unwrap();
                (parent_proofs, proofs, ancestor_headers, provider_db)
            })
            .await?;

        // Get the contracts from the initial db.
        let mut contracts = HashSet::new();
        let initial_db = provider_db.initial_db;
        for account in initial_db.accounts.values() {
            let code = &account.info.code;
            if let Some(code) = code {
                contracts.insert(code.bytecode.0.clone());
            }
        }

        // Construct the state trie and storage from the proofs.
        let (state_trie, storage) =
            proofs_to_tries(input.parent_header.state_root, parent_proofs, proofs)?;

        // Create the block builder input
        let input = SP1RethInput {
            parent_state_trie: state_trie,
            parent_storage: storage,
            contracts: contracts.into_iter().map(Bytes).collect(),
            ancestor_headers,
            ..input
        };

        // DONE!

        Ok(input)
    }
}

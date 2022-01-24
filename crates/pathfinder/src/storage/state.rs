use anyhow::Context;
use pedersen::StarkHash;
use rusqlite::{named_params, OptionalExtension, Transaction};
use web3::types::H256;

use crate::{
    core::{
        ContractHash, ContractRoot, ContractStateHash, EthereumBlockHash, EthereumBlockNumber,
        EthereumLogIndex, EthereumTransactionHash, EthereumTransactionIndex, GlobalRoot,
        StarknetBlockHash, StarknetBlockNumber,
    },
    storage::{DB_VERSION_CURRENT, DB_VERSION_EMPTY},
};

/// Migrates [GlobalStateTable] and [ContractsStateTable] to the [current version](DB_VERSION_CURRENT).
pub fn migrate(transaction: &Transaction, from_version: u32) -> anyhow::Result<()> {
    GlobalStateTable::migrate(transaction, from_version)
        .context("Failed to migrate the global state table")?;
    ContractsStateTable::migrate(transaction, from_version)
        .context("Failed to migrate the contracts state table")
}

/// Stores descriptions of the global StarkNet state. This data contains
/// StarkNet block metadata as well as the origin point on Ethereum.
///
/// For more specific information, see [GlobalStateTable].
pub struct GlobalStateTable {}

/// A StarkNet global state record from [GlobalStateTable] along with the Ethereum
/// point of origin for this record.
#[derive(Debug, Clone, PartialEq)]
pub struct GlobalStateRecord {
    /// The StarkNet block number of this state.
    pub block_number: StarknetBlockNumber,
    /// The StarkNet block hash of this state.
    pub block_hash: StarknetBlockHash,
    /// The StarkNet global root of this state.
    pub global_root: GlobalRoot,
    /// The Ethereum block number this StarkNet state was confirmed on.
    pub eth_block_number: EthereumBlockNumber,
    /// The Ethereum block hash this StarkNet state was confirmed on.
    pub eth_block_hash: EthereumBlockHash,
    /// The Ethereum transaction's hash this StarkNet state was confirmed on.
    pub eth_tx_hash: EthereumTransactionHash,
    /// The Ethereum transaction's index this StarkNet state was confirmed on.
    pub eth_tx_index: EthereumTransactionIndex,
    /// StarkNet state updates are emitted as log events by the Ethereum StarkNet core contract.
    /// This is the log index linked to this StarkNet state.
    pub eth_log_index: EthereumLogIndex,
}

impl GlobalStateTable {
    /// Migrates the [GlobalStateTable] from the given version to [DB_VERSION_CURRENT].
    fn migrate(transaction: &Transaction, from_version: u32) -> anyhow::Result<()> {
        match from_version {
            DB_VERSION_EMPTY => {} // Fresh database, continue to create table.
            DB_VERSION_CURRENT => return Ok(()), // Table is already correct.
            other => anyhow::bail!("Unknown database version: {}", other),
        }

        // TODO: consider ON DELETE CASCADE when we start cleaning up. Don't forget to document if we use it.
        transaction.execute(
            r"CREATE TABLE global_state (
                    starknet_block_hash       BLOB PRIMARY KEY,
                    starknet_block_number     INTEGER NOT NULL,
                    starknet_global_root      BLOB NOT NULL,
                    ethereum_transaction_hash BLOB NOT NULL,
                    ethereum_log_index        INTEGER NOT NULL,

                    FOREIGN KEY(ethereum_transaction_hash) REFERENCES ethereum_transactions(ethereum_transaction_hash)
                )",
            [],
        )?;

        Ok(())
    }

    /// Inserts a new StarkNet global state.
    ///
    /// Does nothing if the [StarkNet block hash](StarknetBlockHash) already exists.
    ///
    /// Note that the [EthereumTransactionHash] must reference a valid transaction hash
    /// stored in [EthereumTransactionsTable](crate::storage::EthereumTransactionsTable).
    pub fn insert(
        transaction: &Transaction,
        block_number: StarknetBlockNumber,
        block_hash: StarknetBlockHash,
        global_root: GlobalRoot,
        eth_transaction: EthereumTransactionHash,
        eth_log_index: EthereumLogIndex,
    ) -> anyhow::Result<()> {
        transaction.execute(
            r"INSERT INTO global_state (
                    starknet_block_number,
                    starknet_block_hash,
                    starknet_global_root,
                    ethereum_transaction_hash,
                    ethereum_log_index
                ) VALUES (
                    :starknet_block_number,
                    :starknet_block_hash,
                    :starknet_global_root,
                    :ethereum_transaction_hash,
                    :ethereum_log_index
                ) ON CONFLICT DO NOTHING
            ",
            named_params! {
                    ":starknet_block_number": block_number.0,
                    ":starknet_block_hash": &block_hash.0.to_be_bytes()[..],
                    ":starknet_global_root": &global_root.0.to_be_bytes()[..],
                    ":ethereum_transaction_hash": eth_transaction.0.as_bytes(),
                    ":ethereum_log_index": eth_log_index.0,
            },
        )?;
        Ok(())
    }

    /// Retrieves the latest global StarkNet state from the [GlobalStateTable]. Latest is defined as the
    /// record with the largest [StarknetBlockNumber].
    pub fn get_latest_state(
        transaction: &Transaction,
    ) -> anyhow::Result<Option<GlobalStateRecord>> {
        let row = transaction
            .query_row(
                r"SELECT * FROM global_state
                    NATURAL JOIN ethereum_transactions
                    NATURAL JOIN ethereum_blocks
                    ORDER BY starknet_block_number DESC
                    LIMIT 1",
                [],
                |row| {
                    let block_number = StarknetBlockNumber(row.get("starknet_block_number")?);
                    let eth_block_number = EthereumBlockNumber(row.get("ethereum_block_number")?);
                    let tx_index = EthereumTransactionIndex(row.get("ethereum_transaction_index")?);
                    let log_index = EthereumLogIndex(row.get("ethereum_log_index")?);

                    // Unfortunately there is no way to return a non-rusqlite error here so can't convert these yet.
                    let block_hash: Vec<u8> = row.get("starknet_block_hash")?;
                    let root: Vec<u8> = row.get("starknet_global_root")?;
                    let eth_block_hash: Vec<u8> = row.get("ethereum_block_hash")?;
                    let tx_hash: Vec<u8> = row.get("ethereum_transaction_hash")?;

                    Ok((
                        block_number,
                        block_hash,
                        root,
                        eth_block_number,
                        eth_block_hash,
                        tx_hash,
                        tx_index,
                        log_index,
                    ))
                },
            )
            .optional()?;

        let row = row.map(
            |(
                block_number,
                block_hash,
                global_root,
                eth_block_number,
                eth_block_hash,
                eth_tx_hash,
                eth_tx_index,
                eth_log_index,
            )|
             -> anyhow::Result<GlobalStateRecord> {
                let block_hash = StarkHash::from_be_slice(&block_hash)
                    .context("Failed to parse StarkNet block hash")?;
                let global_root = StarkHash::from_be_slice(&global_root)
                    .context("Failed to parse StarkNet global state root")?;

                fn vec_to_h256(bytes: Vec<u8>) -> anyhow::Result<H256> {
                    let bytes: [u8; 32] = match bytes.try_into() {
                        Ok(bytes) => bytes,
                        Err(bad_len) => {
                            anyhow::bail!("Expected exactly 32 bytes but got {}", bad_len.len())
                        }
                    };
                    Ok(H256(bytes))
                }

                let eth_tx_hash = vec_to_h256(eth_tx_hash)
                    .context("Failed to parse Ethereum transaction hash")?;
                let eth_block_hash =
                    vec_to_h256(eth_block_hash).context("Failed to parse Ethereum block hash")?;

                Ok(GlobalStateRecord {
                    block_number,
                    block_hash: StarknetBlockHash(block_hash),
                    global_root: GlobalRoot(global_root),
                    eth_block_number,
                    eth_block_hash: EthereumBlockHash(eth_block_hash),
                    eth_tx_hash: EthereumTransactionHash(eth_tx_hash),
                    eth_tx_index,
                    eth_log_index,
                })
            },
        );

        row.transpose()
    }
}

/// Stores the contract state hash along with its preimage. This is useful to
/// map between the global state tree and the contracts tree.
///
/// Specifically it stores
///
/// - [contract state hash](ContractStateHash)
/// - [contract hash](ContractHash)
/// - [contract root](ContractRoot)
pub struct ContractsStateTable {}

impl ContractsStateTable {
    /// Migrates the [ContractsStateTable] from the given version to [DB_VERSION_CURRENT].
    fn migrate(transaction: &Transaction, from_version: u32) -> anyhow::Result<()> {
        match from_version {
            DB_VERSION_EMPTY => {} // Fresh database, continue to create table.
            DB_VERSION_CURRENT => return Ok(()), // Table is already correct.
            other => anyhow::bail!("Unknown database version: {}", other),
        }

        transaction.execute(
            r"CREATE TABLE contract_states (
                    state_hash BLOB PRIMARY KEY,
                    hash       BLOB NOT NULL,
                    root       BLOB NOT NULL
                )",
            [],
        )?;

        Ok(())
    }

    /// Insert a state hash into the table. Does nothing if the state hash already exists.
    pub fn insert(
        transaction: &Transaction,
        state_hash: ContractStateHash,
        hash: ContractHash,
        root: ContractRoot,
    ) -> anyhow::Result<()> {
        transaction.execute(
            r"INSERT INTO contract_states ( state_hash,  hash,  root)
                                       VALUES (:state_hash, :hash, :root)",
            named_params! {
                ":state_hash": &state_hash.0.to_be_bytes()[..],
                ":hash": &hash.0.to_be_bytes()[..],
                ":root": &root.0.to_be_bytes()[..],
            },
        )?;
        Ok(())
    }

    /// Gets the root associated with the given state hash, or [None]
    /// if it does not exist.
    pub fn get_root(
        transaction: &Transaction,
        state_hash: ContractStateHash,
    ) -> anyhow::Result<Option<ContractRoot>> {
        let bytes: Option<Vec<u8>> = transaction
            .query_row(
                "SELECT root FROM contract_states WHERE state_hash = :state_hash",
                named_params! {
                    ":state_hash": &state_hash.0.to_be_bytes()[..]
                },
                |row| row.get("root"),
            )
            .optional()?;

        let bytes = match bytes {
            Some(bytes) => bytes,
            None => return Ok(None),
        };

        let bytes: [u8; 32] = match bytes.try_into() {
            Ok(bytes) => bytes,
            Err(bytes) => anyhow::bail!("Bad contract root length: {}", bytes.len()),
        };

        let root = StarkHash::from_be_bytes(bytes)?;
        let root = ContractRoot(root);

        Ok(Some(root))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod global {
        use std::str::FromStr;

        use crate::storage::{self};

        use super::*;

        mod insert {
            use super::*;

            #[test]
            fn fails_if_eth_origin_missing() {
                let mut conn = rusqlite::Connection::open_in_memory().unwrap();
                let transaction = conn.transaction().unwrap();

                // The table is joined with the Ethereum block and transaction tables,
                // so we have to create the full database.
                storage::migrate_database(&transaction).unwrap();

                GlobalStateTable::insert(
                    &transaction,
                    StarknetBlockNumber(10),
                    StarknetBlockHash(StarkHash::from_hex_str("123").unwrap()),
                    GlobalRoot(StarkHash::from_hex_str("111").unwrap()),
                    EthereumTransactionHash(H256::from_str(&"abca".repeat(64 / 4)).unwrap()),
                    EthereumLogIndex(99),
                )
                .unwrap_err();
            }
        }

        mod get_latest {
            use super::*;

            #[test]
            fn some() {
                let mut conn = rusqlite::Connection::open_in_memory().unwrap();
                let transaction = conn.transaction().unwrap();

                // Data to insert
                let first = GlobalStateRecord {
                    block_number: StarknetBlockNumber(10),
                    block_hash: StarknetBlockHash(StarkHash::from_hex_str("123").unwrap()),
                    global_root: GlobalRoot(StarkHash::from_hex_str("111").unwrap()),
                    eth_block_number: EthereumBlockNumber(2003),
                    eth_block_hash: EthereumBlockHash(
                        H256::from_str(&"abca".repeat(64 / 4)).unwrap(),
                    ),
                    eth_tx_hash: EthereumTransactionHash(
                        H256::from_str(&"defa".repeat(64 / 4)).unwrap(),
                    ),
                    eth_tx_index: EthereumTransactionIndex(14),
                    eth_log_index: EthereumLogIndex(99),
                };

                let second = GlobalStateRecord {
                    block_number: StarknetBlockNumber(11),
                    block_hash: StarknetBlockHash(StarkHash::from_hex_str("3512234").unwrap()),
                    global_root: GlobalRoot(StarkHash::from_hex_str("9371").unwrap()),
                    eth_block_number: EthereumBlockNumber(98123),
                    eth_block_hash: EthereumBlockHash(
                        H256::from_str(&"267ddfec".repeat(64 / 8)).unwrap(),
                    ),
                    eth_tx_hash: EthereumTransactionHash(
                        H256::from_str(&"897ffeda".repeat(64 / 8)).unwrap(),
                    ),
                    eth_tx_index: EthereumTransactionIndex(84),
                    eth_log_index: EthereumLogIndex(31004),
                };

                let third = GlobalStateRecord {
                    block_number: StarknetBlockNumber(12),
                    block_hash: StarknetBlockHash(StarkHash::from_hex_str("35aac12234").unwrap()),
                    global_root: GlobalRoot(StarkHash::from_hex_str("937addd1").unwrap()),
                    eth_block_number: EthereumBlockNumber(11298123),
                    eth_block_hash: EthereumBlockHash(
                        H256::from_str(&"333eefec".repeat(64 / 8)).unwrap(),
                    ),
                    eth_tx_hash: EthereumTransactionHash(
                        H256::from_str(&"333ffeda".repeat(64 / 8)).unwrap(),
                    ),
                    eth_tx_index: EthereumTransactionIndex(84),
                    eth_log_index: EthereumLogIndex(31004),
                };

                // The table is joined with the Ethereum block and transaction tables,
                // so we have to create the full database.
                storage::migrate_database(&transaction).unwrap();

                // Insert Ethereum data
                storage::EthereumBlocksTable::insert(
                    &transaction,
                    first.eth_block_hash,
                    first.eth_block_number,
                )
                .unwrap();
                storage::EthereumBlocksTable::insert(
                    &transaction,
                    second.eth_block_hash,
                    second.eth_block_number,
                )
                .unwrap();
                storage::EthereumBlocksTable::insert(
                    &transaction,
                    third.eth_block_hash,
                    third.eth_block_number,
                )
                .unwrap();

                storage::EthereumTransactionsTable::insert(
                    &transaction,
                    first.eth_block_hash,
                    first.eth_tx_hash,
                    first.eth_tx_index,
                )
                .unwrap();
                storage::EthereumTransactionsTable::insert(
                    &transaction,
                    second.eth_block_hash,
                    second.eth_tx_hash,
                    second.eth_tx_index,
                )
                .unwrap();
                storage::EthereumTransactionsTable::insert(
                    &transaction,
                    third.eth_block_hash,
                    third.eth_tx_hash,
                    third.eth_tx_index,
                )
                .unwrap();

                // Insert StarkNet state data out of order.
                GlobalStateTable::insert(
                    &transaction,
                    first.block_number,
                    first.block_hash,
                    first.global_root,
                    first.eth_tx_hash,
                    first.eth_log_index,
                )
                .unwrap();
                GlobalStateTable::insert(
                    &transaction,
                    third.block_number,
                    third.block_hash,
                    third.global_root,
                    third.eth_tx_hash,
                    third.eth_log_index,
                )
                .unwrap();
                GlobalStateTable::insert(
                    &transaction,
                    second.block_number,
                    second.block_hash,
                    second.global_root,
                    second.eth_tx_hash,
                    second.eth_log_index,
                )
                .unwrap();

                let latest = GlobalStateTable::get_latest_state(&transaction).unwrap();
                assert_eq!(latest, Some(third));
            }

            #[test]
            fn none() {
                let mut conn = rusqlite::Connection::open_in_memory().unwrap();
                let transaction = conn.transaction().unwrap();

                storage::migrate_database(&transaction).unwrap();

                let latest = GlobalStateTable::get_latest_state(&transaction).unwrap();
                assert_eq!(latest, None);
            }
        }
    }

    mod contracts {
        use super::*;

        #[test]
        fn get_root() {
            let mut conn = rusqlite::Connection::open_in_memory().unwrap();
            let transaction = conn.transaction().unwrap();

            ContractsStateTable::migrate(&transaction, DB_VERSION_EMPTY).unwrap();

            let state_hash = ContractStateHash(StarkHash::from_hex_str("abc").unwrap());
            let hash = ContractHash(StarkHash::from_hex_str("123").unwrap());
            let root = ContractRoot(StarkHash::from_hex_str("def").unwrap());

            ContractsStateTable::insert(&transaction, state_hash, hash, root).unwrap();

            let result = ContractsStateTable::get_root(&transaction, state_hash).unwrap();

            assert_eq!(result, Some(root));
        }
    }
}
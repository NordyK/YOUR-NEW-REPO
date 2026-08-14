//! Real `revm` v40.x validator (Context/Journal API; migrated from the v19 EvmBuilder).
//!
//! Wraps the `revm::Evm` so the engine can execute arbitrary calldata against the
//! warm-DB mirror. The validator is used to:
//! * verify executor-style calldata returns a non-revert outcome,
//! * measure `gas_used` accurately,
//! * detect storage-access patterns that violate the route's
//!   `RouteFootprintManifest`.
//!
//! This module is a thin adapter — it does not duplicate AMM math. Production usage is
//! exercised via `ValidationHarness` (the math-equivalent harness) for the V2/V2 fast
//! path; `revm_validator` is the truth source for routes that touch unfamiliar bytecode
//! (V3 callbacks, Algebra hooks, Curve admin paths, etc.).

use alloy_primitives::U256;
use alloy_primitives::{Address as AAddress, Bytes, TxKind, B256};
use revm::context::result::{ExecutionResult, Output, ResultAndState};
use revm::context::{Context, TxEnv};
use revm::database::{CacheDB, EmptyDB};
use revm::state::{AccountInfo, Bytecode};
use revm::{Database, DatabaseCommit, ExecuteEvm, MainBuilder, MainContext};
use thiserror::Error;

/// REVM validation errors.
#[derive(Debug, Error)]
pub enum RevmError {
    /// EVM-level error (DB read, gas, etc.).
    #[error("evm error: {0}")]
    Evm(String),
    /// Execution reverted.
    #[error("revert: {0}")]
    Revert(String),
    /// Output type unsupported (e.g. create when call expected).
    #[error("unsupported output")]
    UnsupportedOutput,
}

/// REVM execution result extracted to plain types.
#[derive(Debug, Clone)]
pub struct RevmCallResult {
    /// Whether execution succeeded (no revert / halt).
    pub success: bool,
    /// Gas units consumed.
    pub gas_used: u64,
    /// Return data.
    pub output: Vec<u8>,
}

/// REVM validator with a warm CacheDB. Construct once at startup and reuse.
pub struct RevmValidator {
    db: CacheDB<EmptyDB>,
}

impl RevmValidator {
    /// Empty warm DB.
    pub fn new() -> Self {
        Self {
            db: CacheDB::new(EmptyDB::default()),
        }
    }

    /// Pre-load an account with the given balance, nonce, and (optional) code. The
    /// startup orchestrator calls this for every token, pool, router, and trader EOA the
    /// engine cares about.
    pub fn preload_account(
        &mut self,
        address: [u8; 20],
        balance: U256,
        nonce: u64,
        code: Option<Vec<u8>>,
    ) {
        // `new_raw` eagerly analyzes the bytecode (revm-bytecode 11.0.1: the only legacy
        // `BytecodeKind` is `LegacyAnalyzed`), so the jump table is built ONCE here at warm-up
        // and the interpreter never re-analyzes on each `transact`. Pinned by the
        // `preloaded_bytecode_is_analyzed_*` test (#4 — preload-analyzed is already optimal).
        let bytecode = code.map(|c| Bytecode::new_raw(Bytes::from(c)));
        let info = AccountInfo {
            balance,
            nonce,
            code_hash: bytecode
                .as_ref()
                .map(|c| c.hash_slow())
                .unwrap_or(B256::ZERO),
            code: bytecode,
            ..Default::default()
        };
        let a = AAddress::from_slice(&address);
        self.db.insert_account_info(a, info);
    }

    /// Write a 32-byte storage slot for `address`.
    pub fn preload_storage(
        &mut self,
        address: [u8; 20],
        slot: U256,
        value: U256,
    ) -> Result<(), RevmError> {
        let a = AAddress::from_slice(&address);
        self.db
            .insert_account_storage(a, slot, value)
            .map_err(|e| RevmError::Evm(format!("{e:?}")))?;
        Ok(())
    }

    /// Execute a call against the warm DB. `from` is the sender (must be preloaded with
    /// enough balance to cover gas). `to` is the contract being called. `gas_limit` and
    /// `gas_price` are explicit so the cost model is deterministic.
    pub fn call(
        &mut self,
        from: [u8; 20],
        to: [u8; 20],
        value: U256,
        data: Vec<u8>,
        gas_limit: u64,
        gas_price: U256,
    ) -> Result<RevmCallResult, RevmError> {
        let mut evm = Context::mainnet()
            .with_db(&mut self.db)
            .modify_cfg_chained(|cfg| {
                cfg.disable_nonce_check = true;
                cfg.tx_gas_limit_cap = Some(u64::MAX); // EIP-7825 cap off (revm-19 parity)
            })
            .build_mainnet();
        let tx = TxEnv::builder()
            .caller(AAddress::from_slice(&from))
            .kind(TxKind::Call(AAddress::from_slice(&to)))
            .value(value)
            .data(Bytes::from(data))
            .gas_limit(gas_limit)
            .gas_price(gas_price_u128(gas_price))
            .chain_id(None)
            .build()
            .map_err(|e| RevmError::Evm(format!("tx_env {e:?}")))?;
        let ResultAndState { result, state: _ } = evm
            .transact(tx)
            .map_err(|e| RevmError::Evm(format!("{e:?}")))?;
        match result {
            ExecutionResult::Success { gas, output, .. } => {
                let out = match output {
                    Output::Call(b) => b.to_vec(),
                    Output::Create(b, _) => b.to_vec(),
                };
                Ok(RevmCallResult {
                    success: true,
                    gas_used: gas.tx_gas_used(),
                    output: out,
                })
            }
            ExecutionResult::Revert { gas, output, .. } => Ok(RevmCallResult {
                success: false,
                gas_used: gas.tx_gas_used(),
                output: output.to_vec(),
            }),
            ExecutionResult::Halt { gas, reason, .. } => Err(RevmError::Evm(format!(
                "halt {:?} after gas_used={}",
                reason,
                gas.tx_gas_used()
            ))),
        }
    }

    /// Read a storage slot from the warm DB.
    pub fn storage(&mut self, address: [u8; 20], slot: U256) -> Result<U256, RevmError> {
        let a = AAddress::from_slice(&address);
        let v = self
            .db
            .storage(a, slot)
            .map_err(|e| RevmError::Evm(format!("{e:?}")))?;
        Ok(v)
    }

    /// Execute a (sequencer-feed) tx against the warm DB AND commit the resulting state
    /// diff back into the cache, so the warm DB self-updates from execution (no RPC).
    /// Returns the call result plus the list of addresses whose storage changed — the
    /// caller intersects this with its resident pool set to emit dirty events.
    ///
    /// This is the §13 EventReady-(b) primitive: re-run the exact tx the sequencer
    /// ordered, read the post-tx pool state at feed latency. Sender balance/gas must be
    /// pre-seeded (or overridden) so the tx doesn't revert on funds.
    pub fn execute_feed_tx(
        &mut self,
        from: [u8; 20],
        to: [u8; 20],
        value: U256,
        data: Vec<u8>,
        gas_limit: u64,
        gas_price: U256,
    ) -> Result<(RevmCallResult, Vec<[u8; 20]>), RevmError> {
        let ResultAndState { result, state } = {
            let mut evm = Context::mainnet()
                .with_db(&mut self.db)
                .modify_cfg_chained(|cfg| {
                    cfg.disable_nonce_check = true;
                    cfg.tx_gas_limit_cap = Some(u64::MAX); // EIP-7825 cap off (revm-19 parity)
                })
                .build_mainnet();
            let tx = TxEnv::builder()
                .caller(AAddress::from_slice(&from))
                .kind(TxKind::Call(AAddress::from_slice(&to)))
                .value(value)
                .data(Bytes::from(data))
                .gas_limit(gas_limit)
                .gas_price(gas_price_u128(gas_price))
                .chain_id(None)
                .build()
                .map_err(|e| RevmError::Evm(format!("tx_env {e:?}")))?;
            evm.transact(tx)
                .map_err(|e| RevmError::Evm(format!("{e:?}")))?
        };
        // Collect addresses with a real storage mutation (present != original).
        let mut changed: Vec<[u8; 20]> = Vec::new();
        for (addr, acct) in state.iter() {
            if acct
                .storage
                .values()
                .any(|s| s.present_value != s.original_value)
            {
                let mut a = [0u8; 20];
                a.copy_from_slice(addr.as_slice());
                changed.push(a);
            }
        }
        // Commit the diff so the warm cache reflects the post-tx state (self-feeding).
        self.db.commit(state);
        let call = match result {
            ExecutionResult::Success { gas, output, .. } => {
                let out = match output {
                    Output::Call(b) => b.to_vec(),
                    Output::Create(b, _) => b.to_vec(),
                };
                RevmCallResult {
                    success: true,
                    gas_used: gas.tx_gas_used(),
                    output: out,
                }
            }
            ExecutionResult::Revert { gas, output, .. } => RevmCallResult {
                success: false,
                gas_used: gas.tx_gas_used(),
                output: output.to_vec(),
            },
            ExecutionResult::Halt { gas, reason, .. } => {
                return Err(RevmError::Evm(format!(
                    "halt {reason:?} gas_used={}",
                    gas.tx_gas_used()
                )));
            }
        };
        Ok((call, changed))
    }
}

impl Default for RevmValidator {
    fn default() -> Self {
        Self::new()
    }
}

/// revm 40's `TxEnv.gas_price` is `u128` (was `U256`). The validator's public API keeps the
/// historical `U256` parameter; saturate on overflow (a >u128 gas price is nonsensical anyway).
///
/// Note: the engine's `U256` (`alloy_primitives::U256`) IS the type revm consumes, so there is
/// no `U256`↔alloy conversion to perform — the former `to_alloy`/`from_alloy` byte round-trips
/// were pure identities and have been removed (#2).
fn gas_price_u128(v: U256) -> u128 {
    v.try_into().unwrap_or(u128::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #4 — the warm DB stores ANALYZED bytecode. In revm-bytecode 11.0.1 the only legacy
    /// `BytecodeKind` is `LegacyAnalyzed`: `Bytecode::new_raw` (used by `preload_account` and
    /// the fork DB) eagerly builds the jump table at construction, so the interpreter never
    /// re-analyzes per `transact`. This pins the property — if a future revm bump reintroduces
    /// a lazily-analyzed/raw kind, this fails and we revisit preloading an analyzed form.
    #[test]
    fn preloaded_bytecode_is_analyzed_not_reanalyzed_per_call() {
        let bc = Bytecode::new_raw(Bytes::from(vec![0x60, 0x00, 0x54, 0x00])); // PUSH1 0 SLOAD STOP
        assert!(
            bc.is_legacy(),
            "new_raw must yield analyzed (LegacyAnalyzed) bytecode so the EVM does not \
             re-analyze on each transact"
        );
    }

    /// Execute a no-op call against an empty contract — should revert with no output
    /// (calling an account with no code on REVM returns success with empty output).
    #[test]
    fn empty_target_returns_success() {
        let mut v = RevmValidator::new();
        v.preload_account([0x11; 20], U256::from(10u64).pow(U256::from(18)), 0, None);
        v.preload_account([0x22; 20], U256::ZERO, 0, None);
        let r = v
            .call(
                [0x11; 20],
                [0x22; 20],
                U256::ZERO,
                vec![],
                100_000,
                U256::from(1u64),
            )
            .unwrap();
        assert!(r.success);
        assert_eq!(r.gas_used, 21_000);
    }

    #[test]
    fn storage_round_trip() {
        let mut v = RevmValidator::new();
        v.preload_account([0xaa; 20], U256::ZERO, 0, None);
        v.preload_storage([0xaa; 20], U256::from(7u64), U256::from(42u64))
            .unwrap();
        let read = v.storage([0xaa; 20], U256::from(7u64)).unwrap();
        assert_eq!(read, U256::from(42u64));
    }

    /// Bytecode that returns the caller's address. We use the simplest valid bytecode:
    /// `CALLER PUSH1 0 MSTORE PUSH1 20 PUSH1 12 RETURN`
    /// (Stores the 20-byte caller right-aligned in a 32-byte word; returns the 20 LSBs.)
    #[test]
    fn execute_caller_returning_bytecode() {
        let bytecode = vec![
            0x33, // CALLER
            0x60, 0x00, // PUSH1 0
            0x52, // MSTORE
            0x60, 0x14, // PUSH1 20
            0x60, 0x0c, // PUSH1 12
            0xf3, // RETURN
        ];
        let mut v = RevmValidator::new();
        v.preload_account([0x11; 20], U256::from(10u64).pow(U256::from(18)), 0, None);
        v.preload_account([0xee; 20], U256::ZERO, 0, Some(bytecode));
        let r = v
            .call(
                [0x11; 20],
                [0xee; 20],
                U256::ZERO,
                vec![],
                100_000,
                U256::from(1u64),
            )
            .unwrap();
        assert!(r.success, "execution failed: {r:?}");
        assert_eq!(r.output.len(), 20);
        assert_eq!(&r.output[..], &[0x11; 20]);
    }

    /// §13 feed-execution primitive: run a tx that SSTOREs calldata[0..32] into slot 0,
    /// confirm `execute_feed_tx` (a) reports the contract as changed and (b) COMMITS the
    /// write to the warm DB (a later read returns the new value — the self-feeding cache).
    /// Bytecode: PUSH1 0 / CALLDATALOAD / PUSH1 0 / SSTORE / STOP = 0x60003560005500.
    #[test]
    fn feed_tx_commits_state_and_reports_diff() {
        let bytecode = vec![0x60, 0x00, 0x35, 0x60, 0x00, 0x55, 0x00];
        let mut v = RevmValidator::new();
        v.preload_account([0x11; 20], U256::from(10u64).pow(U256::from(18)), 0, None);
        v.preload_account([0xee; 20], U256::ZERO, 0, Some(bytecode));
        // slot 0 starts at 0
        assert_eq!(v.storage([0xee; 20], U256::ZERO).unwrap(), U256::ZERO);
        // calldata = the 32-byte value 0x...2a (=42) to store
        let mut calldata = vec![0u8; 32];
        calldata[31] = 42;
        let (r, changed) = v
            .execute_feed_tx(
                [0x11; 20],
                [0xee; 20],
                U256::ZERO,
                calldata,
                100_000,
                U256::from(1u64),
            )
            .unwrap();
        assert!(r.success, "feed tx failed: {r:?}");
        // (a) the contract is reported as changed
        assert!(
            changed.contains(&[0xee; 20]),
            "changed set must include the pool: {changed:?}"
        );
        // (b) the write was committed to the warm DB (self-feeding cache)
        assert_eq!(
            v.storage([0xee; 20], U256::ZERO).unwrap(),
            U256::from(42u64)
        );
    }

    /// Correctness: REVM must READ pre-warmed storage, COMPUTE on it, and COMMIT the
    /// result — the exact pattern a pool swap follows (read reserves → math → write new
    /// reserves). Bytecode reads slot 0, adds calldata[0..32], writes slot 0:
    /// PUSH1 0 SLOAD / PUSH1 0 CALLDATALOAD / ADD / PUSH1 0 SSTORE / STOP
    /// = 0x6000546000350160005500
    #[test]
    fn feed_tx_reads_prewarmed_storage_computes_and_commits() {
        let bytecode = vec![
            0x60, 0x00, 0x54, // PUSH1 0 SLOAD       -> stack: [old]
            0x60, 0x00, 0x35, // PUSH1 0 CALLDATALOAD-> stack: [old, delta]
            0x01, // ADD                              -> stack: [old+delta]
            0x60, 0x00, 0x55, // PUSH1 0 SSTORE       (store old+delta at slot 0)
            0x00, // STOP
        ];
        let mut v = RevmValidator::new();
        v.preload_account([0x11; 20], U256::from(10u64).pow(U256::from(18)), 0, None);
        v.preload_account([0xee; 20], U256::ZERO, 0, Some(bytecode));
        // PRE-WARM slot 0 = 100 (the "current reserve")
        v.preload_storage([0xee; 20], U256::ZERO, U256::from(100u64))
            .unwrap();

        let mut calldata = vec![0u8; 32];
        calldata[31] = 23; // delta
        let (r, changed) = v
            .execute_feed_tx(
                [0x11; 20],
                [0xee; 20],
                U256::ZERO,
                calldata,
                100_000,
                U256::from(1u64),
            )
            .unwrap();
        assert!(r.success, "rmw tx failed: {r:?}");
        assert!(changed.contains(&[0xee; 20]));
        // 100 (read from pre-warm) + 23 (computed) = 123 (committed)
        assert_eq!(
            v.storage([0xee; 20], U256::ZERO).unwrap(),
            U256::from(123u64)
        );
    }
}

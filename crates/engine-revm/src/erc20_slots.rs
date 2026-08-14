//! Compute (and discover) ERC-20 storage slots so the REVM validator's prewarm can cache the
//! `balanceOf`/`allowance` words a route will read — closing the largest remaining cold-miss class
//! (TAIL_LATENCY_PLAN §2 A1). Mapping slots are keccak-derived, so for a *known* holder they are
//! fully computable offline; the only unknown is each token's mapping base slot, which differs by
//! implementation (OpenZeppelin `_balances` at 0, USDC's FiatToken proxy at 9, USDT at 2, …).
//!
//! [`find_erc20_balance_base_slot`] discovers that base slot empirically at boot by trying a small
//! candidate set and matching against the token's own `balanceOf` — so no hand-maintained per-token
//! table can silently rot, and a token whose layout we can't resolve is simply skipped (it then
//! falls back to a lazy cold fetch, never a wrong value).
//!
//! Solidity layout reference: a `mapping(K => V)` declared at slot `p` stores `m[k]` at
//! `keccak256(abi.encode(k, p))`; nested mappings hash twice. (docs.soliditylang.org, layout in
//! storage.)

use alloy_primitives::{keccak256, U256};

/// Left-pad a 20-byte address into a 32-byte ABI word.
#[inline]
fn word_from_address(addr: [u8; 20]) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..32].copy_from_slice(&addr);
    w
}

/// Storage slot of `balances[holder]` for an ERC-20 whose balance mapping is declared at
/// `base_slot`: `keccak256(pad32(holder) ++ pad32(base_slot))`.
pub fn erc20_balance_slot(holder: [u8; 20], base_slot: U256) -> U256 {
    let mut buf = [0u8; 64];
    buf[..32].copy_from_slice(&word_from_address(holder));
    buf[32..].copy_from_slice(&base_slot.to_be_bytes::<32>());
    U256::from_be_bytes(keccak256(buf).0)
}

/// Storage slot of `allowance[owner][spender]` for an ERC-20 whose allowance mapping is declared at
/// `base_slot`: the outer key (`owner`) hashes against `base_slot`, then the inner key (`spender`)
/// hashes against that result — `keccak256(pad32(spender) ++ keccak256(pad32(owner) ++ pad32(base)))`.
pub fn erc20_allowance_slot(owner: [u8; 20], spender: [u8; 20], base_slot: U256) -> U256 {
    let mut outer = [0u8; 64];
    outer[..32].copy_from_slice(&word_from_address(owner));
    outer[32..].copy_from_slice(&base_slot.to_be_bytes::<32>());
    let inner_base = keccak256(outer);
    let mut inner = [0u8; 64];
    inner[..32].copy_from_slice(&word_from_address(spender));
    inner[32..].copy_from_slice(inner_base.as_slice());
    U256::from_be_bytes(keccak256(inner).0)
}

/// Candidate mapping base slots to probe when discovering a token's `balanceOf` layout, ordered by
/// prevalence (OpenZeppelin/most tokens at 0/1; USDT at 2; assorted proxies at 3, 9; a few higher).
pub const COMMON_BALANCE_BASE_SLOTS: &[u64] = &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 51];

/// Discover the balance-mapping base slot for a token by matching the computed slot's stored value
/// against the token's own `balanceOf(holder)`.
///
/// `expected_balance` MUST be non-zero (a zero balance can't disambiguate — almost every slot reads
/// zero), so call with a holder known to hold the token (e.g. a pool that trades it). Returns the
/// first candidate base slot whose derived storage slot holds exactly `expected_balance`, or `None`
/// if none match (the token is then skipped by the prewarm and falls back to lazy fetch). `None` for
/// a zero `expected_balance` by construction.
pub fn find_erc20_balance_base_slot<F>(
    holder: [u8; 20],
    expected_balance: U256,
    candidate_base_slots: &[u64],
    mut read_storage: F,
) -> Option<u64>
where
    F: FnMut(U256) -> U256,
{
    if expected_balance.is_zero() {
        return None;
    }
    candidate_base_slots.iter().copied().find(|&base| {
        read_storage(erc20_balance_slot(holder, U256::from(base))) == expected_balance
    })
}

/// A read/warm interface over a fork-backed cache, abstracted so the prewarm orchestration in
/// [`prewarm_token_balance_slots`] is unit-testable with a mock and shares one code path with the
/// real `ForkExecutor` at boot.
pub trait StorageProbe {
    /// `token.balanceOf(holder)`, or `None` if the call reverts / can't be decoded.
    fn token_balance_of(&mut self, token: [u8; 20], holder: [u8; 20]) -> Option<U256>;
    /// Read one storage slot of `contract` (through the warm cache → fork).
    fn read_storage_slot(&mut self, contract: [u8; 20], slot: U256) -> U256;
    /// Force `contract`'s `slot` into the warm cache (so a later validation read is a cache hit).
    fn warm_storage_slot(&mut self, contract: [u8; 20], slot: U256);
}

/// A token plus the holders whose `balanceOf` slots a route may read (the pools that trade it, the
/// flash Vault, the executor). `holders` should contain at least one address with a non-zero
/// balance so the token's mapping base slot can be discovered.
#[derive(Debug, Clone)]
pub struct TokenHolders {
    /// ERC-20 token address.
    pub token: [u8; 20],
    /// Addresses whose balance slots to warm for this token.
    pub holders: Vec<[u8; 20]>,
}

/// Outcome of a balance-slot prewarm pass — the boot self-audit (TAIL_LATENCY_PLAN §2 A1 gate).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PrewarmAudit {
    /// Tokens considered.
    pub tokens_seen: u32,
    /// Tokens whose balance-mapping base slot was discovered (and slots warmed).
    pub tokens_resolved: u32,
    /// Tokens skipped because no candidate base slot matched (fall back to lazy fetch — never wrong).
    pub tokens_unresolved: u32,
    /// Total balance storage slots warmed across all resolved tokens.
    pub balance_slots_warmed: u32,
}

/// Discover each token's balance-mapping base slot (by matching `balanceOf` against derived storage,
/// [`find_erc20_balance_base_slot`]) and warm the `balanceOf` slot of every listed holder. A token
/// whose base slot can't be resolved is left for lazy fetch (correct, just not pre-warmed). Returns
/// the audit counts. Pure orchestration over [`StorageProbe`] — the same path runs in tests (mock)
/// and at boot (`ForkExecutor`).
pub fn prewarm_token_balance_slots<P: StorageProbe>(
    probe: &mut P,
    tokens: &[TokenHolders],
    candidate_base_slots: &[u64],
) -> PrewarmAudit {
    let mut audit = PrewarmAudit::default();
    for th in tokens {
        audit.tokens_seen += 1;
        // Pick a discovery holder with a non-zero balance (zero can't disambiguate the base slot).
        let discovery = th.holders.iter().copied().find_map(|h| {
            probe
                .token_balance_of(th.token, h)
                .filter(|b| !b.is_zero())
                .map(|b| (h, b))
        });
        let Some((dh, bal)) = discovery else {
            audit.tokens_unresolved += 1;
            continue;
        };
        let token = th.token;
        let base = find_erc20_balance_base_slot(dh, bal, candidate_base_slots, |slot| {
            probe.read_storage_slot(token, slot)
        });
        let Some(base) = base else {
            audit.tokens_unresolved += 1;
            continue;
        };
        audit.tokens_resolved += 1;
        let base = U256::from(base);
        for h in &th.holders {
            probe.warm_storage_slot(token, erc20_balance_slot(*h, base));
            audit.balance_slots_warmed += 1;
        }
    }
    audit
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn balance_slot_matches_published_golden_vector() {
        // Authoritative vector (Solidity storage-layout reference, balancesMappingIndex=0):
        // key 0x6827b8f6cc60497d9bf5210d602C0EcaFDF7C405 → slot
        // 0x86dfc0930cb222883cc0138873d68c1c9864fc2fe59d208c17f3484f489bef04.
        let holder: [u8; 20] = [
            0x68, 0x27, 0xb8, 0xf6, 0xcc, 0x60, 0x49, 0x7d, 0x9b, 0xf5, 0x21, 0x0d, 0x60, 0x2c,
            0x0e, 0xca, 0xfd, 0xf7, 0xc4, 0x05,
        ];
        let want: [u8; 32] = [
            0x86, 0xdf, 0xc0, 0x93, 0x0c, 0xb2, 0x22, 0x88, 0x3c, 0xc0, 0x13, 0x88, 0x73, 0xd6,
            0x8c, 0x1c, 0x98, 0x64, 0xfc, 0x2f, 0xe5, 0x9d, 0x20, 0x8c, 0x17, 0xf3, 0x48, 0x4f,
            0x48, 0x9b, 0xef, 0x04,
        ];
        assert_eq!(
            erc20_balance_slot(holder, U256::ZERO),
            U256::from_be_bytes(want),
            "balance slot must match the published keccak vector"
        );
    }

    #[test]
    fn balance_slot_depends_on_holder_and_base() {
        let a = [0x11u8; 20];
        let b = [0x22u8; 20];
        assert_ne!(
            erc20_balance_slot(a, U256::ZERO),
            erc20_balance_slot(b, U256::ZERO)
        );
        assert_ne!(
            erc20_balance_slot(a, U256::ZERO),
            erc20_balance_slot(a, U256::from(9u64)),
            "different base slot → different storage slot"
        );
    }

    #[test]
    fn allowance_slot_is_double_hash_and_order_sensitive() {
        let owner = [0x11u8; 20];
        let spender = [0x22u8; 20];
        let s1 = erc20_allowance_slot(owner, spender, U256::ZERO);
        // Swapping owner/spender must change the slot (nested mapping is order-sensitive).
        let s2 = erc20_allowance_slot(spender, owner, U256::ZERO);
        assert_ne!(s1, s2);
        // And it must differ from the single-level balance slot at the same base.
        assert_ne!(s1, erc20_balance_slot(owner, U256::ZERO));
    }

    #[test]
    fn finder_discovers_the_base_slot_for_a_token_at_slot_9() {
        // Simulate a USDC-style token: balances mapping at base slot 9, holder has 1_000_000.
        let holder = [0xabu8; 20];
        let bal = U256::from(1_000_000u64);
        let true_slot = erc20_balance_slot(holder, U256::from(9u64));
        let mut storage: HashMap<U256, U256> = HashMap::new();
        storage.insert(true_slot, bal);
        let reader = |slot: U256| storage.get(&slot).copied().unwrap_or(U256::ZERO);

        let found = find_erc20_balance_base_slot(holder, bal, COMMON_BALANCE_BASE_SLOTS, reader);
        assert_eq!(found, Some(9), "must discover the real base slot");
    }

    #[test]
    fn finder_returns_none_when_no_candidate_matches() {
        let holder = [0xcdu8; 20];
        let bal = U256::from(42u64);
        // Storage that holds the balance at an exotic base slot (777) NOT in the candidate set.
        let exotic = erc20_balance_slot(holder, U256::from(777u64));
        let reader = move |slot: U256| if slot == exotic { bal } else { U256::ZERO };
        assert_eq!(
            find_erc20_balance_base_slot(holder, bal, COMMON_BALANCE_BASE_SLOTS, reader),
            None
        );
    }

    #[test]
    fn finder_refuses_zero_balance_ambiguity() {
        let holder = [0xefu8; 20];
        // A reader that returns zero for everything — exactly the ambiguous case.
        let reader = |_slot: U256| U256::ZERO;
        assert_eq!(
            find_erc20_balance_base_slot(holder, U256::ZERO, COMMON_BALANCE_BASE_SLOTS, reader),
            None,
            "zero expected balance must never claim a base slot"
        );
    }

    /// Mock probe: each token has a (base_slot, balances-by-holder); storage reflects those at the
    /// derived slots. Records which (contract, slot) pairs were warmed.
    struct MockProbe {
        /// token → (base_slot, holder→balance)
        tokens: HashMap<[u8; 20], (u64, HashMap<[u8; 20], U256>)>,
        warmed: Vec<([u8; 20], U256)>,
    }
    impl StorageProbe for MockProbe {
        fn token_balance_of(&mut self, token: [u8; 20], holder: [u8; 20]) -> Option<U256> {
            self.tokens
                .get(&token)
                .and_then(|(_b, bals)| bals.get(&holder).copied())
        }
        fn read_storage_slot(&mut self, contract: [u8; 20], slot: U256) -> U256 {
            let Some((base, bals)) = self.tokens.get(&contract) else {
                return U256::ZERO;
            };
            // Reflect each holder's balance at its derived slot for the token's true base.
            for (h, b) in bals {
                if erc20_balance_slot(*h, U256::from(*base)) == slot {
                    return *b;
                }
            }
            U256::ZERO
        }
        fn warm_storage_slot(&mut self, contract: [u8; 20], slot: U256) {
            self.warmed.push((contract, slot));
        }
    }

    #[test]
    fn prewarm_resolves_and_warms_all_holders() {
        let token = [0x11u8; 20];
        let pool_a = [0xa1u8; 20];
        let pool_b = [0xb2u8; 20];
        let vault = [0xc3u8; 20];
        let mut bals = HashMap::new();
        bals.insert(pool_a, U256::from(500u64));
        bals.insert(pool_b, U256::from(900u64));
        bals.insert(vault, U256::from(0u64)); // vault holds none of this token
        let mut tokens = HashMap::new();
        tokens.insert(token, (2u64, bals)); // USDT-style base slot 2
        let mut probe = MockProbe {
            tokens,
            warmed: Vec::new(),
        };

        let th = TokenHolders {
            token,
            holders: vec![vault, pool_a, pool_b], // vault first (zero) — discovery must skip it
        };
        let audit = prewarm_token_balance_slots(&mut probe, &[th], COMMON_BALANCE_BASE_SLOTS);

        assert_eq!(audit.tokens_resolved, 1);
        assert_eq!(audit.tokens_unresolved, 0);
        assert_eq!(audit.balance_slots_warmed, 3, "all three holders warmed");
        // Each warmed slot is the correct derived balance slot at the discovered base (2).
        for h in [vault, pool_a, pool_b] {
            let want = erc20_balance_slot(h, U256::from(2u64));
            assert!(
                probe.warmed.contains(&(token, want)),
                "holder {h:?} balance slot not warmed"
            );
        }
    }

    #[test]
    fn prewarm_skips_token_with_no_nonzero_holder() {
        // All holders report zero balance → base slot can't be discovered → token skipped.
        let token = [0x33u8; 20];
        let h = [0x44u8; 20];
        let mut bals = HashMap::new();
        bals.insert(h, U256::ZERO);
        let mut tokens = HashMap::new();
        tokens.insert(token, (0u64, bals));
        let mut probe = MockProbe {
            tokens,
            warmed: Vec::new(),
        };
        let audit = prewarm_token_balance_slots(
            &mut probe,
            &[TokenHolders {
                token,
                holders: vec![h],
            }],
            COMMON_BALANCE_BASE_SLOTS,
        );
        assert_eq!(audit.tokens_resolved, 0);
        assert_eq!(audit.tokens_unresolved, 1);
        assert_eq!(audit.balance_slots_warmed, 0);
        assert!(
            probe.warmed.is_empty(),
            "nothing warmed for an unresolved token"
        );
    }
}

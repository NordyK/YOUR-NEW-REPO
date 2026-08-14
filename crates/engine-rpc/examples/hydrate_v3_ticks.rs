//! Hydrate canonical V3 tick word0 storage into a saved Anvil load-state.
//!
//! Gate 3 use:
//!   ARBITRUM_RPC_URL=... cargo run -p engine-rpc --release --example hydrate_v3_ticks
//!
//! Optional env:
//!   INPUT_LOAD_STATE=../anvil/results/gate3_parent_block_cache_state_probe/465686728.load_state.json
//!   OUTPUT_LOAD_STATE=../anvil/results/gate3_parent_block_cache_state_probe/465686728.v3ticks.load_state.json
//!   PRE_STATE_BLOCK=465686728
//!   HYDRATE_TICK_WIDTH=400

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::PathBuf;

use anyhow::{Context, Result};
use engine_feed::Address;
use engine_rpc::{
    client::{ArbitrumRpcClient, BlockTag},
    erc20_balance_slot, fetch_v3_slot0, fetch_v3_tick_window, hydrate_v3_ticks_in_load_state,
    write_storage_word_in_load_state, BALANCER_VAULT_ARBITRUM,
};

const DEFAULT_INPUT: &str =
    "../anvil/results/gate3_parent_block_cache_state_probe/465686728.load_state.json";
const DEFAULT_OUTPUT: &str =
    "../anvil/results/gate3_parent_block_cache_state_probe/465686728.v3ticks.load_state.json";
const DEFAULT_BLOCK: u64 = 465_686_728;
const DEFAULT_WIDTH: i32 = 400;
const WBTC_ARBITRUM: Address = [
    0x2f, 0x2a, 0x25, 0x43, 0xb7, 0x6a, 0x41, 0x66, 0x54, 0x9f, 0x7a, 0xab, 0x2e, 0x75, 0xbe, 0xf0,
    0xae, 0xfc, 0x5b, 0x0f,
];
const WBTC_BALANCE_MAPPING_SLOT: u64 = 51;

struct PoolSpec {
    name: &'static str,
    address: Address,
    tick_spacing: i32,
    ticks_mapping_slot: u64,
}

fn main() -> Result<()> {
    let rpc_url = std::env::var("ARBITRUM_RPC_URL")
        .or_else(|_| std::env::var("ARCHIVE_RPC_URL"))
        .context("set ARBITRUM_RPC_URL or ARCHIVE_RPC_URL")?;
    let input = env_path("INPUT_LOAD_STATE", DEFAULT_INPUT);
    let output = env_path("OUTPUT_LOAD_STATE", DEFAULT_OUTPUT);
    let block_number = env_u64("PRE_STATE_BLOCK", DEFAULT_BLOCK)?;
    let width = env_i32("HYDRATE_TICK_WIDTH", DEFAULT_WIDTH)?;
    let block = BlockTag::Number(block_number);
    let rpc = ArbitrumRpcClient::new(&rpc_url);

    let mut state: serde_json::Value = serde_json::from_reader(BufReader::new(
        File::open(&input).with_context(|| format!("open input load-state {}", input.display()))?,
    ))
    .with_context(|| format!("parse input load-state {}", input.display()))?;

    let pools = [
        PoolSpec {
            name: "PancakeV3 WETH/WBTC fee100",
            address: addr("4bfc22a4da7f31f8a912a79a7e44a822398b4390"),
            tick_spacing: 1,
            ticks_mapping_slot: 6,
        },
        PoolSpec {
            name: "UniswapV3 WBTC/WETH fee500",
            address: addr("2f5e87c9312fa29aed5c179e456625d79015299c"),
            tick_spacing: 10,
            ticks_mapping_slot: 5,
        },
    ];

    println!(
        "hydrate_v3_ticks input={} output={} block={} width={}",
        input.display(),
        output.display(),
        block_number,
        width
    );
    let mut total_ticks = 0usize;
    for pool in pools {
        let slot0 = fetch_v3_slot0(&rpc, &pool.address, block)
            .with_context(|| format!("fetch slot0 for {}", pool.name))?;
        let ticks = fetch_v3_tick_window(
            &rpc,
            &pool.address,
            slot0.tick,
            pool.tick_spacing,
            width,
            block,
        )
        .with_context(|| format!("fetch tick window for {}", pool.name))?;
        let written = hydrate_v3_ticks_in_load_state(
            &mut state,
            &pool.address,
            pool.ticks_mapping_slot,
            &ticks,
        )
        .with_context(|| format!("hydrate load-state for {}", pool.name))?;
        total_ticks += written;
        println!(
            "  {} address=0x{} centre_tick={} spacing={} ticks_mapping_slot={} initialized_ticks={} storage_words_written={}",
            pool.name,
            faster_hex::hex_string(&pool.address),
            slot0.tick,
            pool.tick_spacing,
            pool.ticks_mapping_slot,
            ticks.len(),
            written
        );
    }
    let wbtc_balance_slot = erc20_balance_slot(&BALANCER_VAULT_ARBITRUM, WBTC_BALANCE_MAPPING_SLOT);
    let wbtc_balance_word = rpc
        .storage_at(&WBTC_ARBITRUM, &wbtc_balance_slot, block)
        .context("fetch WBTC Balancer Vault balance storage")?;
    write_storage_word_in_load_state(
        &mut state,
        &WBTC_ARBITRUM,
        wbtc_balance_slot,
        wbtc_balance_word,
    )
    .context("hydrate WBTC Balancer Vault balance storage")?;
    println!(
        "  Balancer Vault WBTC balance storage token=0x{} owner=0x{} balance_slot={} storage_key=0x{} storage_word=0x{}",
        faster_hex::hex_string(&WBTC_ARBITRUM),
        faster_hex::hex_string(&BALANCER_VAULT_ARBITRUM),
        WBTC_BALANCE_MAPPING_SLOT,
        faster_hex::hex_string(&wbtc_balance_slot),
        faster_hex::hex_string(&wbtc_balance_word)
    );

    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create output directory {}", parent.display()))?;
        }
    }
    serde_json::to_writer(
        BufWriter::new(
            File::create(&output)
                .with_context(|| format!("create output load-state {}", output.display()))?,
        ),
        &state,
    )
    .with_context(|| format!("write output load-state {}", output.display()))?;
    println!("hydrate_v3_ticks complete total_storage_words_written={total_ticks}");
    Ok(())
}

fn env_path(name: &str, default: &str) -> PathBuf {
    std::env::var(name)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(default))
}

fn env_u64(name: &str, default: u64) -> Result<u64> {
    match std::env::var(name) {
        Ok(v) => v
            .parse()
            .with_context(|| format!("parse {name}={v} as u64")),
        Err(_) => Ok(default),
    }
}

fn env_i32(name: &str, default: i32) -> Result<i32> {
    match std::env::var(name) {
        Ok(v) => v
            .parse()
            .with_context(|| format!("parse {name}={v} as i32")),
        Err(_) => Ok(default),
    }
}

fn addr(s: &str) -> Address {
    let s = s.strip_prefix("0x").unwrap_or(s);
    let mut out = [0u8; 20];
    faster_hex::hex_decode(s.as_bytes(), &mut out).expect("valid hard-coded address");
    out
}

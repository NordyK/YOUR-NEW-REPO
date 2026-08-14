//! Classifier + confidence chain + negative-filter cascade.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod classifier;
pub mod confidence;
pub mod create2;
pub mod filters;
pub mod oneinch;
pub mod router_decoder;
pub mod selectors;
pub mod state_predictor;
pub mod strategy_executor;
pub mod universal_router;

pub use classifier::{Classifier, ClassifierBuilder, TxClass};
pub use confidence::{Confidence, ConfidenceFactors, CONFIDENCE_FLOOR};
pub use filters::{
    depth_upper_passes, fee_floor_passes, marginal_rate_product_v2v2, marginal_rate_product_v3v3,
    marginal_rate_ratio_q32_v3v3, route_passes_filters, FilterReason, FilterResult,
};
pub use oneinch::decode_oneinch;
pub use router_decoder::{
    decode_aggregator_intents, decode_fluid_pool_swap, decode_router_intent, decode_v2_pool_swap,
    decode_v3_multicall, decode_v3_multicall_with_deadline, decode_v3_pool_swap, DecodeError,
    DecodedHop, Direction, RouteIntent, MAX_DECODED_HOPS,
};
pub use selectors::{
    SelectorClass, SelectorRegistry, KNOWN_AGGREGATOR_SELECTORS, KNOWN_V2_ROUTER_SELECTORS,
    KNOWN_V3_ROUTER_SELECTORS,
};
pub use state_predictor::{
    predict_route, PoolPrediction, PredictionError, PredictorContext, MAX_DIRTY_POOLS,
};
pub use strategy_executor::{
    decode_pool_index, resolve_executor_pool, SELECTOR_B2460C48, STRATEGY_EXECUTOR,
};
pub use universal_router::decode_universal_router;

pub mod api;
pub mod engine;
pub mod reporter;
pub mod state;
pub mod types;

/// Target trader: DrPufferfish — high-volume sports bettor
pub const TRADER_ADDRESS: &str = "0xdb27bf2ac5d428a9c63dbc914611036855a6c56e";

/// Polymarket data API base URL (public, no auth required)
pub const DATA_API_BASE: &str = "https://data-api.polymarket.com";

/// RTDS WebSocket URL (real-time data service)
pub const RTDS_WS_URL: &str = "wss://ws-live-data.polymarket.com";

/// CLOB REST API base URL (Central Limit Order Book)
pub const CLOB_API_BASE: &str = "https://clob.polymarket.com";

/// CLOB WebSocket base URL (Central Limit Order Book)
/// Append /market or /user for specific channels
pub const CLOB_WS_MARKET_URL: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/market";
pub const CLOB_WS_USER_URL: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/user";

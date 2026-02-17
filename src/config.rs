use anyhow::Result;
use polymarket_client_sdk::clob::types::OrderType;
use std::env;

use polymarket_client_sdk::types::Address;

/// 解析套利订单类型：GTC、GTD、FOK、FAK，大小写不敏感，无效或未知值默认 GTD。
fn parse_arbitrage_order_type(s: &str) -> OrderType {
    match s.trim().to_uppercase().as_str() {
        "GTC" => OrderType::GTC,
        "GTD" => OrderType::GTD,
        "FOK" => OrderType::FOK,
        "FAK" => OrderType::FAK,
        _ => OrderType::GTD,
    }
}

/// 解析滑点数组：逗号分隔，如 "-0.02,0.0"。
/// 索引 0=上涨/持平侧滑点，1=仅下降侧滑点。只写一个值时用于两项。默认 "0,0.01"。
fn parse_slippage(s: &str) -> [f64; 2] {
    let parts: Vec<f64> = s
        .split(',')
        .map(|x| x.trim().parse().unwrap_or(0.0))
        .collect();
    match parts.len() {
        0 => [0.0, 0.01],
        1 => [parts[0], parts[0]],
        _ => [parts[0], parts[1]],
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub private_key: String,
    pub proxy_address: Option<Address>, // Polymarket Proxy地址（如果使用Email/Magic或Browser Wallet登录）
    pub min_profit_threshold: f64,
    pub max_order_size_usdc: f64,
    pub crypto_symbols: Vec<String>,
    pub market_refresh_advance_secs: u64,
    pub risk_max_exposure_usdc: f64,
    pub risk_imbalance_threshold: f64,
    pub hedge_take_profit_pct: f64, // 对冲止盈百分比（例如0.05表示5%）
    pub hedge_stop_loss_pct: f64,   // 对冲止损百分比（例如0.05表示5%）
    pub arbitrage_execution_spread: f64, // 套利执行价差：yes+no <= 1 - 套利执行价差时，执行套利
    /// 滑点 [first, second]：仅下降侧用 second，上涨与持平用 first。如 "-0.02,0.0"
    pub slippage: [f64; 2],
    pub gtd_expiration_secs: u64, // GTD订单过期时间（秒），默认300秒（5分钟）；仅当 arbitrage_order_type=GTD 时有效
    /// 套利下单时的订单类型：GTC（一直有效）、GTD（配合 gtd_expiration_secs）、FOK（立即全部成交否则取消）、FAK（立即部分成交其余取消）
    pub arbitrage_order_type: OrderType,
    pub stop_arbitrage_before_end_minutes: u64, // 市场结束前N分钟停止执行套利，默认0（不停止）
    /// 定时 Merge 间隔（分钟），0 表示不启用。CONDITION_ID 与订单簿一样由当前窗口市场获取。
    pub merge_interval_minutes: u64,
    /// YES 价格阈值：只有当 YES 价格 >= 此阈值时才执行套利，默认 0.0（不限制）
    pub min_yes_price_threshold: f64,
    /// NO 价格阈值：只有当 NO 价格 >= 此阈值时才执行套利，默认 0.0（不限制）
    pub min_no_price_threshold: f64,
    /// 持仓同步间隔（秒），默认10秒（从API获取最新持仓覆盖本地缓存）
    pub position_sync_interval_secs: u64,
    /// 仓位平衡检查间隔（秒），默认60秒
    pub position_balance_interval_secs: u64,
    /// 不平衡阈值，只有当持仓差异 >= 此阈值时才取消挂单，默认2.0
    pub position_balance_threshold: f64,
    /// 最小总持仓要求，只有当总持仓 >= 此值时才执行平衡，默认5.0
    pub position_balance_min_total: f64,
    /// 窗口结束前收尾：距离当前5分钟窗口结束还有多少分钟时触发收尾（取消挂单→Merge→市价卖剩余）。0=不启用。
    pub wind_down_before_window_end_minutes: u64,
    /// 收尾时单腿卖出的限价单价格（尽量快速成交），默认0.01
    pub wind_down_sell_price: f64,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        dotenvy::dotenv().ok();

        // 解析proxy_address（可选）
        let proxy_address: Option<Address> = env::var("POLYMARKET_PROXY_ADDRESS")
            .ok()
            .and_then(|addr| addr.parse().ok());

        Ok(Config {
            private_key: env::var("POLYMARKET_PRIVATE_KEY")
                .expect("POLYMARKET_PRIVATE_KEY must be set"),
            proxy_address,
            min_profit_threshold: env::var("MIN_PROFIT_THRESHOLD")
                .unwrap_or_else(|_| "0.001".to_string())
                .parse()
                .unwrap_or(0.001),
            max_order_size_usdc: env::var("MAX_ORDER_SIZE_USDC")
                .unwrap_or_else(|_| "100.0".to_string())
                .parse()
                .unwrap_or(100.0),
            crypto_symbols: env::var("CRYPTO_SYMBOLS")
                .unwrap_or_else(|_| "btc,eth,xrp,sol".to_string())
                .split(',')
                .map(|s| s.trim().to_lowercase())
                .collect(),
            market_refresh_advance_secs: env::var("MARKET_REFRESH_ADVANCE_SECS")
                .unwrap_or_else(|_| "5".to_string())
                .parse()
                .unwrap_or(5),
            risk_max_exposure_usdc: env::var("RISK_MAX_EXPOSURE_USDC")
                .unwrap_or_else(|_| "1000.0".to_string())
                .parse()
                .unwrap_or(1000.0),
            risk_imbalance_threshold: env::var("RISK_IMBALANCE_THRESHOLD")
                .unwrap_or_else(|_| "0.1".to_string())
                .parse()
                .unwrap_or(0.1),
            hedge_take_profit_pct: env::var("HEDGE_TAKE_PROFIT_PCT")
                .unwrap_or_else(|_| "0.05".to_string())
                .parse()
                .unwrap_or(0.05), // 默认5%止盈
            hedge_stop_loss_pct: env::var("HEDGE_STOP_LOSS_PCT")
                .unwrap_or_else(|_| "0.05".to_string())
                .parse()
                .unwrap_or(0.05), // 默认5%止损
            arbitrage_execution_spread: env::var("ARBITRAGE_EXECUTION_SPREAD")
                .unwrap_or_else(|_| "0.01".to_string())
                .parse()
                .unwrap_or(0.01), // 默认0.01
            slippage: parse_slippage(&env::var("SLIPPAGE").unwrap_or_else(|_| "0,0.01".to_string())),
            gtd_expiration_secs: env::var("GTD_EXPIRATION_SECS")
                .unwrap_or_else(|_| "300".to_string())
                .parse()
                .unwrap_or(300), // 默认300秒（5分钟）
            arbitrage_order_type: parse_arbitrage_order_type(
                &env::var("ARBITRAGE_ORDER_TYPE").unwrap_or_else(|_| "GTD".to_string()),
            ),
            stop_arbitrage_before_end_minutes: env::var("STOP_ARBITRAGE_BEFORE_END_MINUTES")
                .unwrap_or_else(|_| "0".to_string())
                .parse()
                .unwrap_or(0), // 默认0（不停止）
            merge_interval_minutes: env::var("MERGE_INTERVAL_MINUTES")
                .unwrap_or_else(|_| "0".to_string())
                .parse()
                .unwrap_or(0), // 0=不启用
            min_yes_price_threshold: env::var("MIN_YES_PRICE_THRESHOLD")
                .unwrap_or_else(|_| "0.0".to_string())
                .parse()
                .unwrap_or(0.0), // 默认0.0（不限制）
            min_no_price_threshold: env::var("MIN_NO_PRICE_THRESHOLD")
                .unwrap_or_else(|_| "0.0".to_string())
                .parse()
                .unwrap_or(0.0), // 默认0.0（不限制）
            position_sync_interval_secs: env::var("POSITION_SYNC_INTERVAL_SECS")
                .unwrap_or_else(|_| "10".to_string())
                .parse()
                .unwrap_or(10), // 默认10秒
            position_balance_interval_secs: env::var("POSITION_BALANCE_INTERVAL_SECS")
                .unwrap_or_else(|_| "60".to_string())
                .parse()
                .unwrap_or(60), // 默认60秒
            position_balance_threshold: env::var("POSITION_BALANCE_THRESHOLD")
                .unwrap_or_else(|_| "2.0".to_string())
                .parse()
                .unwrap_or(2.0), // 默认2.0
            position_balance_min_total: env::var("POSITION_BALANCE_MIN_TOTAL")
                .unwrap_or_else(|_| "5.0".to_string())
                .parse()
                .unwrap_or(5.0), // 默认5.0
            wind_down_before_window_end_minutes: env::var("WIND_DOWN_BEFORE_WINDOW_END_MINUTES")
                .unwrap_or_else(|_| "0".to_string())
                .parse()
                .unwrap_or(0), // 0=不启用
            wind_down_sell_price: env::var("WIND_DOWN_SELL_PRICE")
                .unwrap_or_else(|_| "0.01".to_string())
                .parse()
                .unwrap_or(0.01), // 默认0.01
        })
    }
}

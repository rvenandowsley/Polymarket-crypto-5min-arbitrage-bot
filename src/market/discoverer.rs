use anyhow::Result;
use chrono::{DateTime, Utc};
use polymarket_client_sdk::gamma::{Client, types::request::MarketsRequest};
use polymarket_client_sdk::types::{B256, U256};
use tracing::{info, warn};

/// 5分钟窗口的秒数（供 main 等模块计算 window_end 使用）
pub const FIVE_MIN_SECS: i64 = 300;

#[derive(Debug, Clone)]
pub struct MarketInfo {
    pub market_id: B256,
    pub slug: String,
    pub yes_token_id: U256,
    pub no_token_id: U256,
    pub title: String,
    pub end_date: DateTime<Utc>,
    pub crypto_symbol: String,
}

pub struct MarketDiscoverer {
    gamma_client: Client,
    crypto_symbols: Vec<String>,
}

impl MarketDiscoverer {
    pub fn new(crypto_symbols: Vec<String>) -> Self {
        Self {
            gamma_client: Client::default(),
            crypto_symbols,
        }
    }

    /// 计算当前5分钟窗口的开始时间戳（UTC）
    /// 窗口对齐到每5分钟整点：0, 5, 10, 15, 20, 25, 30, 35, 40, 45, 50, 55 分
    pub fn calculate_current_window_timestamp(now: DateTime<Utc>) -> i64 {
        let ts = now.timestamp();
        (ts / FIVE_MIN_SECS) * FIVE_MIN_SECS
    }

    /// 计算下一个5分钟窗口的开始时间戳（UTC）
    pub fn calculate_next_window_timestamp(now: DateTime<Utc>) -> i64 {
        let ts = now.timestamp();
        ((ts / FIVE_MIN_SECS) + 1) * FIVE_MIN_SECS
    }

    /// 生成市场slug列表
    /// 5分钟市场格式：btc-updown-5m-1770972300
    pub fn generate_market_slugs(&self, timestamp: i64) -> Vec<String> {
        self.crypto_symbols
            .iter()
            .map(|symbol| format!("{}-updown-5m-{}", symbol, timestamp))
            .collect()
    }

    /// 获取指定时间戳的5分钟市场
    pub async fn get_markets_for_timestamp(&self, timestamp: i64) -> Result<Vec<MarketInfo>> {
        // 生成所有加密货币的slug
        let slugs = self.generate_market_slugs(timestamp);

        info!(timestamp, slug_count = slugs.len(), "查询市场");

        // 使用Gamma API批量查询
        let request = MarketsRequest::builder()
            .slug(slugs.clone())
            .build();

        match self.gamma_client.markets(&request).await {
            Ok(markets) => {
                // 过滤并解析市场
                let valid_markets: Vec<MarketInfo> = markets
                    .into_iter()
                    .filter_map(|market| self.parse_market(market))
                    .collect();

                info!(count = valid_markets.len(), "找到符合条件的市场");
                Ok(valid_markets)
            }
            Err(e) => {
                warn!(error = %e, timestamp = timestamp, "查询市场失败，可能市场尚未创建");
                Ok(Vec::new())
            }
        }
    }

    /// 解析市场信息，提取YES和NO的token_id
    fn parse_market(&self, market: polymarket_client_sdk::gamma::types::response::Market) -> Option<MarketInfo> {
        // 检查市场是否活跃、启用订单簿且接受订单
        if !market.active.unwrap_or(false) 
           || !market.enable_order_book.unwrap_or(false)
           || !market.accepting_orders.unwrap_or(false) {
            return None;
        }

        // 检查outcomes是否为["Up", "Down"]
        let outcomes = market.outcomes.as_ref()?;

        if outcomes.len() != 2 
           || !outcomes.contains(&"Up".to_string()) 
           || !outcomes.contains(&"Down".to_string()) {
            return None;
        }

        // 获取clobTokenIds
        let token_ids = market.clob_token_ids.as_ref()?;

        if token_ids.len() != 2 {
            return None;
        }

        // 第一个是"Up"的token_id，第二个是"Down"的token_id
        let yes_token_id = token_ids[0];
        let no_token_id = token_ids[1];

        // 获取conditionId
        let market_id = market.condition_id?;

        // 从slug中提取加密货币符号
        let slug = market.slug.as_ref()?;
        let crypto_symbol = slug
            .split('-')
            .next()
            .unwrap_or("")
            .to_string();

        // 获取endDate
        let end_date = market.end_date?;

        Some(MarketInfo {
            market_id,
            slug: slug.clone(),
            yes_token_id,
            no_token_id,
            title: market.question.unwrap_or_default(),
            end_date,
            crypto_symbol,
        })
    }
}

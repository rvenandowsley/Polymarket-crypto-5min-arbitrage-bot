use anyhow::Result;
use dashmap::DashMap;
use futures::Stream;
use futures::StreamExt;
use polymarket_client_sdk::clob::ws::{Client as WsClient, types::response::BookUpdate};
use polymarket_client_sdk::types::{B256, U256};
use std::collections::HashMap;
use std::pin::Pin;
use tracing::{debug, info};

use crate::market::MarketInfo;

/// 缩短 B256 用于日志：保留 0x + 前 8 位 hex，如 0xb91126b7..
#[inline]
fn short_b256(b: &B256) -> String {
    let s = format!("{b}");
    if s.len() > 12 { format!("{}..", &s[..10]) } else { s }
}

/// 缩短 U256 用于日志：保留末尾 8 位，如 ..67033653
#[inline]
fn short_u256(u: &U256) -> String {
    let s = format!("{u}");
    if s.len() > 12 {
        format!("..{}", &s[s.len().saturating_sub(8)..])
    } else {
        s
    }
}

pub struct OrderBookMonitor {
    ws_client: WsClient,
    books: DashMap<U256, BookUpdate>,
    market_map: HashMap<B256, (U256, U256)>, // market_id -> (yes_token_id, no_token_id)
}

pub struct OrderBookPair {
    pub yes_book: BookUpdate,
    pub no_book: BookUpdate,
    pub market_id: B256,
}

impl OrderBookMonitor {
    pub fn new() -> Self {
        Self {
            // 使用未认证的客户端：订单簿订阅不需要认证，这是公开数据
            // 只有订阅用户数据（如用户订单、交易等）才需要认证
            ws_client: WsClient::default(),
            books: DashMap::new(),
            market_map: HashMap::new(),
        }
    }

    /// 订阅新市场
    pub fn subscribe_market(&mut self, market: &MarketInfo) -> Result<()> {
        // 记录市场映射
        self.market_map.insert(
            market.market_id,
            (market.yes_token_id, market.no_token_id),
        );

        info!(
            market_id = short_b256(&market.market_id),
            yes = short_u256(&market.yes_token_id),
            no = short_u256(&market.no_token_id),
            "订阅市场订单簿"
        );

        Ok(())
    }

    /// 创建订单簿订阅流
    /// 
    /// 注意：订单簿订阅使用未认证的 WebSocket 客户端，因为订单簿数据是公开的。
    /// 只有订阅用户相关数据（如用户订单状态、交易历史等）才需要认证。
    pub fn create_orderbook_stream(
        &self,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<BookUpdate>> + Send + '_>>> {
        // 收集所有需要订阅的token_id
        let token_ids: Vec<U256> = self
            .market_map
            .values()
            .flat_map(|(yes, no)| [*yes, *no])
            .collect();

        if token_ids.is_empty() {
            return Err(anyhow::anyhow!("没有市场需要订阅"));
        }

        info!(token_count = token_ids.len(), "创建订单簿订阅流（未认证）");

        // subscribe_orderbook 不需要认证，使用未认证客户端即可
        let stream = self.ws_client.subscribe_orderbook(token_ids)?;
        // 将 SDK 的 Error 转换为 anyhow::Error
        let stream = stream.map(|result| result.map_err(|e| anyhow::anyhow!("{}", e)));
        Ok(Box::pin(stream))
    }

    /// 处理订单簿更新
    pub fn handle_book_update(&self, book: BookUpdate) -> Option<OrderBookPair> {

        // 打印前5档买卖价格（用于调试）
        if !book.bids.is_empty() {
            let top_bids: Vec<String> = book.bids.iter()
                .take(5)
                .map(|b| format!("{}@{}", b.size, b.price))
                .collect();
            debug!(
                asset_id = %book.asset_id,
                "买盘前5档: {}",
                top_bids.join(", ")
            );
        }
        if !book.asks.is_empty() {
            let top_asks: Vec<String> = book.asks.iter()
                .take(5)
                .map(|a| format!("{}@{}", a.size, a.price))
                .collect();
            debug!(
                asset_id = short_u256(&book.asset_id),
                "卖盘前5档: {}",
                top_asks.join(", ")
            );
        }

        // 更新订单簿缓存
        self.books.insert(book.asset_id, book.clone());

        // 查找这个 token 属于哪个市场；任一侧（YES 或 NO）更新都返回 OrderBookPair，以便及时反应套利
        for (market_id, (yes_token, no_token)) in &self.market_map {
            if book.asset_id == *yes_token {
                if let Some(no_book) = self.books.get(no_token) {
                    return Some(OrderBookPair {
                        yes_book: book.clone(),
                        no_book: no_book.clone(),
                        market_id: *market_id,
                    });
                }
            } else if book.asset_id == *no_token {
                if let Some(yes_book) = self.books.get(yes_token) {
                    return Some(OrderBookPair {
                        yes_book: yes_book.clone(),
                        no_book: book.clone(),
                        market_id: *market_id,
                    });
                }
            }
        }

        None
    }

    /// 获取订单簿（如果存在）
    pub fn get_book(&self, token_id: U256) -> Option<BookUpdate> {
        self.books.get(&token_id).map(|b| b.clone())
    }

    /// 清除所有订阅
    pub fn clear(&mut self) {
        self.books.clear();
        self.market_map.clear();
    }
}

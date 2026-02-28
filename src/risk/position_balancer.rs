//! 仓位平衡器：定时检查持仓和挂单，取消多余挂单以保持平衡

use anyhow::Result;
use polymarket_client_sdk::clob::Client;
use polymarket_client_sdk::clob::types::request::OrdersRequest;
use polymarket_client_sdk::clob::types::Side;
use polymarket_client_sdk::types::{B256, Decimal, U256};
use rust_decimal_macros::dec;
use std::collections::HashMap;
use tracing::{debug, error, info, warn};

use super::positions::PositionTracker;
use crate::config::Config as BotConfig;
use poly_5min_bot::positions::get_positions;

/// 仓位平衡器
pub struct PositionBalancer {
    clob_client: Client<polymarket_client_sdk::auth::state::Authenticated<polymarket_client_sdk::auth::Normal>>,
    position_tracker: std::sync::Arc<PositionTracker>,
    threshold: Decimal,
    min_total: Decimal,
    max_order_size: Decimal,
}

impl PositionBalancer {
    pub fn new(
        clob_client: Client<polymarket_client_sdk::auth::state::Authenticated<polymarket_client_sdk::auth::Normal>>,
        position_tracker: std::sync::Arc<PositionTracker>,
        config: &BotConfig,
    ) -> Self {
        Self {
            clob_client,
            position_tracker,
            threshold: Decimal::try_from(config.position_balance_threshold).unwrap_or(dec!(2.0)),
            min_total: Decimal::try_from(config.position_balance_min_total).unwrap_or(dec!(5.0)),
            max_order_size: Decimal::try_from(config.max_order_size_usdc).unwrap_or(dec!(5.0)),
        }
    }

    /// 检查并平衡仓位：获取持仓和挂单，分析每个市场的YES/NO平衡情况，取消多余挂单
    pub async fn check_and_balance_positions(
        &self,
        market_map: &HashMap<B256, (U256, U256)>, // condition_id -> (yes_token_id, no_token_id)
    ) -> Result<()> {
        // 获取所有活跃订单（处理分页）
        let mut all_orders = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let page = self
                .clob_client
                .orders(&OrdersRequest::default(), cursor)
                .await?;
            
            all_orders.extend(page.data);
            
            if page.next_cursor.is_empty() || page.next_cursor == "LTE=" {
                break;
            }
            cursor = Some(page.next_cursor);
        }

        if all_orders.is_empty() {
            debug!("没有活跃订单，跳过仓位平衡检查");
            return Ok(());
        }

        // 获取持仓（从PositionTracker，已通过定时同步更新）
        let positions = get_positions().await?;

        // 按市场分组订单和持仓
        let mut market_data: HashMap<B256, MarketBalanceData> = HashMap::new();

        // 初始化市场数据
        for (condition_id, (yes_token, no_token)) in market_map {
            market_data.insert(*condition_id, MarketBalanceData {
                condition_id: *condition_id,
                yes_token_id: *yes_token,
                no_token_id: *no_token,
                yes_position: dec!(0),
                no_position: dec!(0),
                yes_orders: Vec::new(),
                no_orders: Vec::new(),
            });
        }

        // 填充持仓数据
        for pos in positions {
            if let Some(data) = market_data.get_mut(&pos.condition_id) {
                // outcome_index: 0=YES, 1=NO
                if pos.outcome_index == 0 {
                    data.yes_position = pos.size;
                } else if pos.outcome_index == 1 {
                    data.no_position = pos.size;
                }
            }
        }

        // 填充订单数据
        for order in all_orders {
            // 只处理买入订单（Side::Buy）
            if order.side != Side::Buy {
                continue;
            }

            // 找到订单所属的市场
            for data in market_data.values_mut() {
                if order.asset_id == data.yes_token_id {
                    let pending_size = order.original_size - order.size_matched;
                    if pending_size > dec!(0) {
                        data.yes_orders.push(OrderInfo {
                            order_id: order.id.clone(),
                            price: order.price,
                            pending_size,
                        });
                    }
                } else if order.asset_id == data.no_token_id {
                    let pending_size = order.original_size - order.size_matched;
                    if pending_size > dec!(0) {
                        data.no_orders.push(OrderInfo {
                            order_id: order.id.clone(),
                            price: order.price,
                            pending_size,
                        });
                    }
                }
            }
        }

        // 对每个市场进行平衡检查
        for data in market_data.values() {
            if let Err(e) = self.balance_market(data).await {
                warn!(error = %e, "❌ 市场仓位平衡失败");
            }
        }

        Ok(())
    }

    /// 平衡单个市场
    async fn balance_market(&self, data: &MarketBalanceData) -> Result<()> {
        // 计算实际持仓差异
        let position_diff = (data.yes_position - data.no_position).abs();

        // 计算挂单数量
        let yes_pending: Decimal = data.yes_orders.iter().map(|o| o.pending_size).sum();
        let no_pending: Decimal = data.no_orders.iter().map(|o| o.pending_size).sum();

        // 计算总持仓
        let yes_total = data.yes_position + yes_pending;
        let no_total = data.no_position + no_pending;
        let total = yes_total + no_total;

        // 如果总持仓小于最小要求，跳过
        if total < self.min_total {
            debug!("总持仓 {} 小于最小要求 {}，跳过平衡", total, self.min_total);
            return Ok(());
        }

        // 情况1：实际持仓已失衡（不含挂单）
        if position_diff >= self.threshold {
            if data.yes_position > data.no_position {
                // YES过多，取消所有YES挂单，取消对应数量的NO挂单
                let cancel_yes_order_ids: Vec<String> = data.yes_orders.iter().map(|o| o.order_id.clone()).collect();
                let cancel_yes_count = cancel_yes_order_ids.len();
                
                // 计算需要取消的NO挂单数量：min(no_pending, yes_pending)
                let cancel_no_size = yes_pending.min(no_pending);

                if cancel_yes_count > 0 || cancel_no_size > dec!(0) {
                    info!(
                        "⚠️ 检测到YES持仓过多 | YES持仓:{} NO持仓:{} | 取消 {} 个YES订单和约 {} 份NO挂单",
                        data.yes_position,
                        data.no_position,
                        cancel_yes_count,
                        cancel_no_size
                    );

                    // 取消YES订单
                    if cancel_yes_count > 0 {
                        let yes_order_ids: Vec<&str> = cancel_yes_order_ids.iter().map(|s| s.as_str()).collect();
                        if let Err(e) = self.clob_client.cancel_orders(&yes_order_ids).await {
                            error!(error = %e, "❌ 取消YES订单失败");
                        } else {
                            info!("✅ 已取消 {} 个YES订单", cancel_yes_count);
                        }
                    }

                    // 取消NO订单（按价格排序，取消价格最低的，直到累计数量达到cancel_no_size）
                    if cancel_no_size > dec!(0) {
                        let mut no_orders_sorted = data.no_orders.clone();
                        no_orders_sorted.sort_by(|a, b| a.price.partial_cmp(&b.price).unwrap_or(std::cmp::Ordering::Equal));
                        
                        let mut cancel_no_order_ids = Vec::new();
                        let mut accumulated_size = dec!(0);
                        
                        for order in no_orders_sorted {
                            if accumulated_size >= cancel_no_size {
                                break;
                            }
                            cancel_no_order_ids.push(order.order_id.clone());
                            accumulated_size += order.pending_size;
                        }
                        
                        if !cancel_no_order_ids.is_empty() {
                            let cancel_no_order_ids_ref: Vec<&str> = cancel_no_order_ids.iter().map(|s| s.as_str()).collect();
                            if let Err(e) = self.clob_client.cancel_orders(&cancel_no_order_ids_ref).await {
                                error!(error = %e, "取消NO订单失败");
                            } else {
                                info!("已取消 {} 个NO订单（累计 {} 份）", cancel_no_order_ids.len(), accumulated_size);
                            }
                        }
                    }
                }
            } else {
                // NO过多，取消所有NO挂单，取消对应数量的YES挂单
                let cancel_no_order_ids: Vec<String> = data.no_orders.iter().map(|o| o.order_id.clone()).collect();
                let cancel_no_count = cancel_no_order_ids.len();
                
                // 计算需要取消的YES挂单数量：min(yes_pending, no_pending)
                let cancel_yes_size = no_pending.min(yes_pending);

                if cancel_no_count > 0 || cancel_yes_size > dec!(0) {
                    info!(
                        "⚠️ 检测到NO持仓过多 | YES持仓:{} NO持仓:{} | 取消 {} 个NO订单和约 {} 份YES挂单",
                        data.yes_position,
                        data.no_position,
                        cancel_no_count,
                        cancel_yes_size
                    );

                    // 取消NO订单
                    if cancel_no_count > 0 {
                        let no_order_ids: Vec<&str> = cancel_no_order_ids.iter().map(|s| s.as_str()).collect();
                        if let Err(e) = self.clob_client.cancel_orders(&no_order_ids).await {
                            error!(error = %e, "取消NO订单失败");
                        } else {
                            info!("已取消 {} 个NO订单", cancel_no_count);
                        }
                    }

                    // 取消YES订单（按价格排序，取消价格最低的，直到累计数量达到cancel_yes_size）
                    if cancel_yes_size > dec!(0) {
                        let mut yes_orders_sorted = data.yes_orders.clone();
                        yes_orders_sorted.sort_by(|a, b| a.price.partial_cmp(&b.price).unwrap_or(std::cmp::Ordering::Equal));
                        
                        let mut cancel_yes_order_ids = Vec::new();
                        let mut accumulated_size = dec!(0);
                        
                        for order in yes_orders_sorted {
                            if accumulated_size >= cancel_yes_size {
                                break;
                            }
                            cancel_yes_order_ids.push(order.order_id.clone());
                            accumulated_size += order.pending_size;
                        }
                        
                        if !cancel_yes_order_ids.is_empty() {
                            let cancel_yes_order_ids_ref: Vec<&str> = cancel_yes_order_ids.iter().map(|s| s.as_str()).collect();
                            if let Err(e) = self.clob_client.cancel_orders(&cancel_yes_order_ids_ref).await {
                                error!(error = %e, "❌ 取消YES订单失败");
                            } else {
                                info!("✅ 已取消 {} 个YES订单（累计 {} 份）", cancel_yes_order_ids.len(), accumulated_size);
                            }
                        }
                    }
                }
            }
            return Ok(());
        }

        // 情况2：实际持仓平衡，但挂单导致总持仓失衡
        let target = (yes_total + no_total) / dec!(2);
        let yes_imbalance = yes_total - target;
        let no_imbalance = no_total - target;

        // 取消多余的YES订单
        if yes_imbalance.abs() >= self.threshold && yes_imbalance > dec!(0) {
            let mut yes_orders_sorted = data.yes_orders.clone();
            yes_orders_sorted.sort_by(|a, b| a.price.partial_cmp(&b.price).unwrap_or(std::cmp::Ordering::Equal));

            let mut cancel_size = dec!(0);
            let mut cancel_order_ids = Vec::new();
            
            for order in yes_orders_sorted {
                if cancel_size >= yes_imbalance {
                    break;
                }
                cancel_order_ids.push(order.order_id.clone());
                cancel_size += order.pending_size;
            }

            if !cancel_order_ids.is_empty() {
                info!("⚠️ YES挂单过多，取消 {} 个YES订单", cancel_order_ids.len());

                let cancel_order_ids_ref: Vec<&str> = cancel_order_ids.iter().map(|s| s.as_str()).collect();
                if let Err(e) = self.clob_client.cancel_orders(&cancel_order_ids_ref).await {
                    error!(error = %e, "❌ 取消YES订单失败");
                } else {
                    info!("✅ 已取消 {} 个YES订单", cancel_order_ids.len());
                }
            }
        }

        // 取消多余的NO订单
        if no_imbalance.abs() >= self.threshold && no_imbalance > dec!(0) {
            let mut no_orders_sorted = data.no_orders.clone();
            no_orders_sorted.sort_by(|a, b| a.price.partial_cmp(&b.price).unwrap_or(std::cmp::Ordering::Equal));

            let mut cancel_size = dec!(0);
            let mut cancel_order_ids = Vec::new();
            
            for order in no_orders_sorted {
                if cancel_size >= no_imbalance {
                    break;
                }
                cancel_order_ids.push(order.order_id.clone());
                cancel_size += order.pending_size;
            }

            if !cancel_order_ids.is_empty() {
                info!("NO挂单过多，取消 {} 个NO订单", cancel_order_ids.len());

                let cancel_order_ids_ref: Vec<&str> = cancel_order_ids.iter().map(|s| s.as_str()).collect();
                if let Err(e) = self.clob_client.cancel_orders(&cancel_order_ids_ref).await {
                    error!(error = %e, "取消NO订单失败");
                } else {
                    info!("已取消 {} 个NO订单", cancel_order_ids.len());
                }
            }
        }

        Ok(())
    }

    /// 检查指定市场是否应该跳过套利（如果已严重不平衡）
    /// 使用本地缓存的持仓数据，零延迟
    pub fn should_skip_arbitrage(&self, yes_token: U256, no_token: U256) -> bool {
        let (yes_pos, no_pos) = self.position_tracker.get_pair_positions(yes_token, no_token);
        let position_diff = (yes_pos - no_pos).abs();

        if position_diff >= self.threshold {
            warn!(
                yes_position = %yes_pos,
                no_position = %no_pos,
                position_diff = %position_diff,
                threshold = %self.threshold,
                "⛔ 持仓已严重不平衡，跳过套利执行"
            );
            return true;
        }

        false
    }
}

/// 市场平衡数据
struct MarketBalanceData {
    condition_id: B256,
    yes_token_id: U256,
    no_token_id: U256,
    yes_position: Decimal,
    no_position: Decimal,
    yes_orders: Vec<OrderInfo>,
    no_orders: Vec<OrderInfo>,
}

/// 订单信息
#[derive(Clone)]
struct OrderInfo {
    order_id: String,
    price: Decimal,
    pending_size: Decimal,
}

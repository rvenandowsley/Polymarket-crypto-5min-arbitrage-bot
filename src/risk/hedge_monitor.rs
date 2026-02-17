use anyhow::Result;
use alloy::signers::Signer;
use alloy::signers::local::LocalSigner;
use dashmap::DashMap;
use polymarket_client_sdk::clob::Client;
use polymarket_client_sdk::clob::types::{OrderType, Side};
use polymarket_client_sdk::clob::ws::types::response::BookUpdate;
use polymarket_client_sdk::types::{Address, Decimal, U256};
use polymarket_client_sdk::POLYGON;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal_macros::dec;
use std::str::FromStr;
use std::sync::Arc;
use tracing::{debug, error, info, trace, warn};

use super::positions::PositionTracker;
use super::recovery::RecoveryAction;

#[derive(Debug, Clone)]
pub struct HedgePosition {
    pub token_id: U256,
    pub opposite_token_id: U256, // 对立边的token_id（用于计算差值）
    pub amount: Decimal,
    pub entry_price: Decimal, // 买入价格（卖一价）
    pub take_profit_price: Decimal, // 止盈价格
    pub stop_loss_price: Decimal,   // 止损价格
    pub pair_id: String,
    pub market_display: String, // 市场显示名称（例如"btc预测市场"）
    pub order_id: Option<String>, // 如果已下GTC订单，保存订单ID
    pub pending_sell_amount: Decimal, // 待卖出的数量
}

pub struct HedgeMonitor {
    client: Client<polymarket_client_sdk::auth::state::Authenticated<polymarket_client_sdk::auth::Normal>>,
    private_key: String,
    proxy_address: Option<Address>,
    positions: DashMap<String, HedgePosition>, // pair_id -> position
    position_tracker: Arc<PositionTracker>, // 用于更新风险敞口
}

impl HedgeMonitor {
    pub fn new(
        client: Client<polymarket_client_sdk::auth::state::Authenticated<polymarket_client_sdk::auth::Normal>>,
        private_key: String,
        proxy_address: Option<Address>,
        position_tracker: Arc<PositionTracker>,
    ) -> Self {
        Self {
            client,
            private_key,
            proxy_address,
            positions: DashMap::new(),
            position_tracker,
        }
    }

    /// 添加需要监测的对冲仓位
    pub fn add_position(&self, action: &RecoveryAction) -> Result<()> {
        if let RecoveryAction::MonitorForExit {
            token_id,
            opposite_token_id,
            amount,
            entry_price,
            take_profit_pct,
            stop_loss_pct,
            pair_id,
            market_display,
        } = action
        {
            // 计算止盈止损价格
            let take_profit_price = *entry_price * (dec!(1.0) + *take_profit_pct);
            let stop_loss_price = *entry_price * (dec!(1.0) - *stop_loss_pct);

            info!(
                "🛡️ 开始对冲监测 | 市场:{} | 持仓:{}份 | 买入价:{:.4} | 止盈:{:.4} | 止损:{:.4}",
                market_display,
                amount,
                entry_price,
                take_profit_price,
                stop_loss_price
            );

            let position = HedgePosition {
                token_id: *token_id,
                opposite_token_id: *opposite_token_id,
                amount: *amount,
                entry_price: *entry_price,
                take_profit_price,
                stop_loss_price,
                pair_id: pair_id.clone(),
                market_display: market_display.clone(),
                order_id: None,
                pending_sell_amount: dec!(0),
            };

            self.positions.insert(pair_id.clone(), position);
        }
        Ok(())
    }

    /// 更新entry_price（从订单簿获取当前卖一价）
    pub fn update_entry_price(&self, pair_id: &str, entry_price: Decimal) {
        if let Some(mut pos) = self.positions.get_mut(pair_id) {
            let old_entry = pos.entry_price;
            pos.entry_price = entry_price;
            // 重新计算止盈止损价格
            let take_profit_pct = (pos.take_profit_price - old_entry) / old_entry;
            let stop_loss_pct = (old_entry - pos.stop_loss_price) / old_entry;
            pos.take_profit_price = entry_price * (dec!(1.0) + take_profit_pct);
            pos.stop_loss_price = entry_price * (dec!(1.0) - stop_loss_pct);
            
            info!(
                pair_id = %pair_id,
                old_entry = %old_entry,
                new_entry = %entry_price,
                take_profit_price = %pos.take_profit_price,
                stop_loss_price = %pos.stop_loss_price,
                "更新买入价格"
            );
        }
    }

    /// 检查订单簿更新，如果达到止盈止损则卖出
    pub async fn check_and_execute(&self, book: &BookUpdate) -> Result<()> {
        // 获取买一价（bids数组最后一个，因为bids是价格降序排列）
        let best_bid = book.bids.last();
        let best_bid_price = match best_bid {
            Some(bid) => bid.price,
            None => return Ok(()), // 没有买盘，无法卖出
        };

        // 查找所有需要监测的仓位
        let positions_to_check: Vec<(String, HedgePosition)> = self
            .positions
            .iter()
            .filter(|entry| entry.value().token_id == book.asset_id)
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect();

        for (pair_id, position) in positions_to_check {
            // 检查是否已经下过GTC订单，如果有则使用订单簿最新价格重新挂单
            if let Some(ref order_id) = position.order_id {
                let pending_amount = position.pending_sell_amount;
                if pending_amount > dec!(0) {
                    // 有未成交的订单，使用订单簿最新价格重新挂单
                    info!(
                        "🔄 检测到未成交订单 | 市场:{} | 订单ID:{} | 剩余:{}份 | 使用新价格:{:.4}重新挂单",
                        position.market_display,
                        &order_id[..16],
                        pending_amount,
                        best_bid_price
                    );
                    // 清除旧订单ID，准备重新挂单
                    if let Some(mut pos) = self.positions.get_mut(&pair_id) {
                        pos.order_id = None;
                    }
                    // 继续执行下面的挂单逻辑，使用pending_amount作为卖出数量
                } else {
                    // 订单已提交但pending_amount为0，可能正在处理中，跳过
                    continue;
                }
            }

            // 检查是否达到止盈或止损
            let (should_sell, reason) = if best_bid_price >= position.take_profit_price {
                let profit_pct = ((best_bid_price - position.entry_price) / position.entry_price * dec!(100.0)).to_f64().unwrap_or(0.0);
                (true, format!("止盈({:.2}%)", profit_pct))
            } else if best_bid_price <= position.stop_loss_price {
                let loss_pct = ((position.entry_price - best_bid_price) / position.entry_price * dec!(100.0)).to_f64().unwrap_or(0.0);
                (true, format!("止损({:.2}%)", loss_pct))
            } else {
                (false, String::new())
            };

            if should_sell {
                // 获取当前token和对立边token的持仓
                let current_position = self.position_tracker.get_position(position.token_id);
                let opposite_position = self.position_tracker.get_position(position.opposite_token_id);
                
                // 计算差值：当前持仓 - 对立边持仓
                let difference = current_position - opposite_position;
                
                // 如果差值 <= 0，说明对立边可以覆盖，不需要卖出
                if difference <= dec!(0) {
                    info!(
                        "⏸️ 无需卖出 | 市场:{} | 当前持仓:{}份 | 对立边持仓:{}份 | 差值:{}份 | 对立边可覆盖",
                        position.market_display,
                        current_position,
                        opposite_position,
                        difference
                    );
                    continue;
                }
                
                // 确定实际要卖出的数量
                let sell_amount = if position.order_id.is_some() && position.pending_sell_amount > dec!(0) {
                    // 如果有未成交订单，使用pending_sell_amount
                    position.pending_sell_amount
                } else {
                    // 否则使用差值
                    difference
                };
                
                // 差值 > 0，只卖出差值部分
                info!(
                    "✅ 达到{} | 市场:{} | 当前买一价:{:.4} | 买入价:{:.4} | 当前持仓:{}份 | 对立边持仓:{}份 | 差值:{}份 | 准备卖出:{}份",
                    reason,
                    position.market_display,
                    best_bid_price,
                    position.entry_price,
                    current_position,
                    opposite_position,
                    difference,
                    sell_amount
                );
                
                // 使用GTC订单卖出
                // 为了避免阻塞主循环，将卖出操作放到独立的异步任务中
                let position_clone = position.clone();
                let pair_id_clone = pair_id.clone();
                let position_tracker = self.position_tracker.clone();
                let positions = self.positions.clone();
                let client = self.client.clone();
                let private_key = self.private_key.clone();
                
                // 先标记为正在处理，避免重复下单（使用remove+insert避免阻塞）
                if let Some((_, mut pos)) = self.positions.remove(&pair_id) {
                    pos.order_id = Some("processing".to_string());
                    self.positions.insert(pair_id.clone(), pos);
                }
                
                tokio::spawn(async move {
                    // 重新创建 signer（因为不能在 spawn 中直接使用 self）
                    let signer = match LocalSigner::from_str(&private_key) {
                        Ok(s) => s.with_chain_id(Some(POLYGON)),
                        Err(e) => {
                            error!(
                                "❌ 创建signer失败 | 市场:{} | 错误:{}",
                                position_clone.market_display,
                                e
                            );
                            return;
                        }
                    };
                    
                    // 执行卖出操作
                    match Self::execute_sell_order(
                        &client,
                        &signer,
                        &position_clone,
                        best_bid_price,
                        sell_amount,
                    ).await {
                        Ok((order_id, filled, remaining)) => {
                            // 更新仓位，标记已下订单（使用remove+insert避免get_mut阻塞）
                            let order_id_short = order_id[..16].to_string();
                            if let Some((_, mut pos)) = positions.remove(&pair_id_clone) {
                                if remaining > dec!(0) {
                                    // 还有剩余，保存订单ID
                                    pos.order_id = Some(order_id);
                                    pos.pending_sell_amount = remaining;
                                    info!("🔒 仓位order_id已更新 | 市场:{} | 订单ID:{} | 剩余:{}份", 
                                        position_clone.market_display, order_id_short, remaining);
                                } else {
                                    // 完全成交，清除订单ID
                                    pos.order_id = None;
                                    pos.pending_sell_amount = dec!(0);
                                    info!("✅ 卖出订单已完全成交 | 市场:{} | 订单ID:{} | 成交:{}份", 
                                        position_clone.market_display, order_id_short, filled);
                                }
                                positions.insert(pair_id_clone.clone(), pos);
                            } else {
                                warn!("⚠️ 未找到仓位 | pair_id:{}", pair_id_clone);
                            }
                            
                            // 只有实际成交的部分才更新持仓和风险敞口
                            if filled > dec!(0) {
                                info!("📊 开始更新持仓 | 市场:{} | 减少:{}份", 
                                    position_clone.market_display, filled);
                                position_tracker.update_position(position_clone.token_id, -filled);
                                info!("📊 持仓更新完成 | 市场:{}", position_clone.market_display);
                                
                                // 更新风险敞口成本
                                info!("💰 开始更新风险敞口 | 市场:{} | entry_price:{} | sell_amount:{}", 
                                    position_clone.market_display,
                                    position_clone.entry_price,
                                    filled);
                                position_tracker.update_exposure_cost(
                                    position_clone.token_id,
                                    position_clone.entry_price,
                                    -filled,
                                );
                                info!("💰 风险敞口更新完成 | 市场:{}", position_clone.market_display);
                                
                                // 计算风险敞口
                                let current_exposure = position_tracker.calculate_exposure();
                                info!(
                                    "📉 风险敞口已更新 | 市场:{} | 卖出:{}份 | 当前敞口:{:.2} USD",
                                    position_clone.market_display,
                                    filled,
                                    current_exposure
                                );
                            }
                        }
                        Err(e) => {
                            error!(
                                "❌ 卖出订单失败 | 市场:{} | 价格:{:.4} | 错误:{}",
                                position_clone.market_display,
                                best_bid_price,
                                e
                            );
                            // 如果失败，清除 processing 标记
                            if let Some(mut pos) = positions.get_mut(&pair_id_clone) {
                                pos.order_id = None;
                            }
                        }
                    }
                });
            }
        }

        Ok(())
    }

    /// 计算实际卖出数量（考虑手续费）
    fn calculate_sell_amount(&self, position: &HedgePosition) -> Decimal {
        self.calculate_sell_amount_with_size(position, position.amount)
    }

    /// 计算指定数量的实际卖出数量（考虑手续费）
    fn calculate_sell_amount_with_size(&self, position: &HedgePosition, base_amount: Decimal) -> Decimal {
        // 计算手续费
        let p = position.entry_price.to_f64().unwrap_or(0.0);
        let c = 100.0;
        let fee_rate = 0.25;
        let exponent = 2.0;
        
        let base = p * (1.0 - p);
        let fee_value = c * fee_rate * base.powf(exponent);
        let fee_decimal = Decimal::try_from(fee_value).unwrap_or(dec!(0));
        
        // 计算实际可用份额
        let available_amount = if fee_decimal >= dec!(100.0) {
            dec!(0.01)
        } else {
            let multiplier = (dec!(100.0) - fee_decimal) / dec!(100.0);
            base_amount * multiplier
        };
        
        // 向下取整到2位小数
        let floored_size = (available_amount * dec!(100.0)).floor() / dec!(100.0);
        
        if floored_size.is_zero() {
            dec!(0.01)
        } else {
            floored_size
        }
    }

    /// 静态方法：计算指定数量的实际卖出数量（考虑手续费）
    fn calculate_sell_amount_static(position: &HedgePosition, base_amount: Decimal) -> Decimal {
        // 计算手续费
        let p = position.entry_price.to_f64().unwrap_or(0.0);
        let c = 100.0;
        let fee_rate = 0.25;
        let exponent = 2.0;
        
        let base = p * (1.0 - p);
        let fee_value = c * fee_rate * base.powf(exponent);
        let fee_decimal = Decimal::try_from(fee_value).unwrap_or(dec!(0));
        
        // 计算实际可用份额
        let available_amount = if fee_decimal >= dec!(100.0) {
            dec!(0.01)
        } else {
            let multiplier = (dec!(100.0) - fee_decimal) / dec!(100.0);
            base_amount * multiplier
        };
        
        // 向下取整到2位小数
        let floored_size = (available_amount * dec!(100.0)).floor() / dec!(100.0);
        
        if floored_size.is_zero() {
            dec!(0.01)
        } else {
            floored_size
        }
    }

    /// 静态方法：执行卖出订单
    async fn execute_sell_order(
        client: &Client<polymarket_client_sdk::auth::state::Authenticated<polymarket_client_sdk::auth::Normal>>,
        signer: &impl Signer<alloy::primitives::Signature>,
        position: &HedgePosition,
        price: Decimal,
        size: Decimal,
    ) -> Result<(String, Decimal, Decimal)> {
        // 计算手续费
        let p = position.entry_price.to_f64().unwrap_or(0.0);
        let c = 100.0;
        let fee_rate = 0.25;
        let exponent = 2.0;
        
        let base = p * (1.0 - p);
        let fee_value = c * fee_rate * base.powf(exponent);
        let fee_decimal = Decimal::try_from(fee_value).unwrap_or(dec!(0));
        
        // 计算实际可用份额
        let available_amount = if fee_decimal >= dec!(100.0) {
            dec!(0.01)
        } else {
            let multiplier = (dec!(100.0) - fee_decimal) / dec!(100.0);
            size * multiplier
        };
        
        // 向下取整到2位小数
        let floored_size = (available_amount * dec!(100.0)).floor() / dec!(100.0);
        let order_size = if floored_size.is_zero() {
            dec!(0.01)
        } else {
            floored_size
        };

        info!(
            "💰 计算卖出份额 | 市场:{} | 基础数量:{:.2}份 | 买入价:{:.4} | 手续费:{:.2}% | 可用份额:{:.2}份 | 下单数量:{:.2}份",
            position.market_display,
            size,
            position.entry_price,
            fee_decimal,
            available_amount,
            order_size
        );

        // 构建GTC卖出订单
        let sell_order = client
            .limit_order()
            .token_id(position.token_id)
            .side(Side::Sell)
            .price(price)
            .size(order_size)
            .order_type(OrderType::GTC)
            .build()
            .await?;

        // 签名订单
        let signed_order = client.sign(signer, sell_order).await?;

        // 提交订单
        let result = client.post_order(signed_order).await?;

        if !result.success {
            let error_msg = result.error_msg.as_deref().unwrap_or("未知错误");
            return Err(anyhow::anyhow!("GTC卖出订单失败: {}", error_msg));
        }

        // 检查订单是否立即成交
        let filled = result.taking_amount;
        let remaining = order_size - filled;
        
        if filled > dec!(0) {
            info!(
                "💰 卖出订单已部分成交 | 市场:{} | 订单ID:{} | 已成交:{}份 | 剩余:{}份",
                position.market_display,
                &result.order_id[..16],
                filled,
                remaining
            );
        } else {
            info!(
                "📋 卖出订单已提交（未立即成交） | 市场:{} | 订单ID:{} | 数量:{}份 | 价格:{:.4}",
                position.market_display,
                &result.order_id[..16],
                order_size,
                price
            );
        }
        
        Ok((result.order_id, filled, remaining))
    }

    /// 使用GTC订单卖出
    /// size: 可选，如果提供则使用该数量，否则使用position.amount
    async fn sell_with_gtc(
        &self,
        position: &HedgePosition,
        price: Decimal,
        size: Option<Decimal>,
    ) -> Result<(String, Decimal, Decimal)> {
        let signer = LocalSigner::from_str(&self.private_key)?
            .with_chain_id(Some(POLYGON));

        // 计算手续费
        // 公式: fee = c * fee_rate * (p * (1-p))^exponent
        // 其中: p = entry_price（买入单价），c = 100（固定值），fee_rate = 0.25，exponent = 2
        // 手续费计算出来是一个介于 0-1.56 之间的浮点数（比例值，不是绝对值）
        let p = position.entry_price.to_f64().unwrap_or(0.0);
        let c = 100.0; // 固定为100
        let fee_rate = 0.25;
        let exponent = 2.0;
        
        // 计算手续费比例值（0-1.56之间）
        let base = p * (1.0 - p);
        let fee_value = c * fee_rate * base.powf(exponent);
        
        // 将手续费转换为 Decimal
        let fee_decimal = Decimal::try_from(fee_value).unwrap_or(dec!(0));
        
        // 使用提供的size，如果没有提供则使用position.amount
        let base_amount = size.unwrap_or(position.amount);
        
        // 计算实际可用份额 = 成交份额 * (100 - Fee) / 100
        // 如果 Fee >= 100，说明异常情况，使用最小可交易单位
        let available_amount = if fee_decimal >= dec!(100.0) {
            dec!(0.01) // 异常情况，使用最小单位
        } else {
            // 正常情况：可用份额 = 成交份额 * (100 - Fee) / 100
            let multiplier = (dec!(100.0) - fee_decimal) / dec!(100.0);
            base_amount * multiplier
        };
        
        // 将订单大小向下取整到2位小数（Polymarket要求）
        // 使用向下取整而不是四舍五入，避免订单大小超过实际持有份额
        // 方法：乘以100，向下取整，再除以100
        let floored_size = (available_amount * dec!(100.0)).floor() / dec!(100.0);
        
        // 如果向下取整后为0，则使用最小可交易单位
        let order_size = if floored_size.is_zero() {
            dec!(0.01) // 最小单位
        } else {
            floored_size
        };

        info!(
            "💰 计算卖出份额 | 市场:{} | 基础数量:{:.2}份 | 买入价:{:.4} | 手续费:{:.2}% | 可用份额:{:.2}份 | 下单数量:{:.2}份",
            position.market_display,
            base_amount,
            position.entry_price,
            fee_decimal,
            available_amount,
            order_size
        );

        // 构建GTC卖出订单
        let sell_order = self
            .client
            .limit_order()
            .token_id(position.token_id)
            .side(Side::Sell)
            .price(price)
            .size(order_size)
            .order_type(OrderType::GTC)
            .build()
            .await?;

        // 签名订单
        let signed_order = self.client.sign(&signer, sell_order).await?;

        // 提交订单
        let result = self.client.post_order(signed_order).await?;

        if !result.success {
            let error_msg = result.error_msg.as_deref().unwrap_or("未知错误");
            return Err(anyhow::anyhow!("GTC卖出订单失败: {}", error_msg));
        }

        // 检查订单是否立即成交
        let filled = result.taking_amount;
        let remaining = order_size - filled;
        
        if filled > dec!(0) {
            info!(
                "💰 卖出订单已部分成交 | 市场:{} | 订单ID:{} | 已成交:{}份 | 剩余:{}份",
                position.market_display,
                &result.order_id[..16],
                filled,
                remaining
            );
        } else {
            info!(
                "📋 卖出订单已提交（未立即成交） | 市场:{} | 订单ID:{} | 数量:{}份 | 价格:{:.4}",
                position.market_display,
                &result.order_id[..16],
                order_size,
                price
            );
        }
        
        Ok((result.order_id, filled, remaining))
    }

    /// 移除已完成的仓位
    pub fn remove_position(&self, pair_id: &str) {
        self.positions.remove(pair_id);
        info!(pair_id = %pair_id, "移除对冲仓位");
    }

    /// 获取所有监测中的仓位
    pub fn get_positions(&self) -> Vec<HedgePosition> {
        self.positions.iter().map(|e| e.value().clone()).collect()
    }
}

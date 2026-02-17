use anyhow::Result;
use polymarket_client_sdk::types::{Decimal, U256};
use rust_decimal_macros::dec;
use tracing::debug;

use super::manager::OrderPair;
use super::positions::PositionTracker;

#[derive(Debug, Clone)]
pub enum RecoveryAction {
    None,
    SellExcess { token_id: String, amount: Decimal },
    MonitorForExit {
        token_id: U256,
        opposite_token_id: U256, // 对立边的token_id（用于计算差值）
        amount: Decimal,
        entry_price: Decimal, // 买入价格（卖一价）
        take_profit_pct: Decimal, // 止盈百分比（例如0.05表示5%）
        stop_loss_pct: Decimal, // 止损百分比（例如0.05表示5%）
        pair_id: String,
        market_display: String, // 市场显示名称（例如"btc预测市场"）
    },
    ManualIntervention { reason: String },
}

pub struct RecoveryStrategy {
    imbalance_threshold: Decimal,
    take_profit_pct: Decimal, // 止盈百分比
    stop_loss_pct: Decimal,   // 止损百分比
}

impl RecoveryStrategy {
    pub fn new(imbalance_threshold: f64, take_profit_pct: f64, stop_loss_pct: f64) -> Self {
        Self {
            imbalance_threshold: Decimal::try_from(imbalance_threshold)
                .unwrap_or(dec!(0.1)),
            take_profit_pct: Decimal::try_from(take_profit_pct)
                .unwrap_or(dec!(0.05)), // 默认5%止盈
            stop_loss_pct: Decimal::try_from(stop_loss_pct)
                .unwrap_or(dec!(0.05)), // 默认5%止损
        }
    }

    /// 处理部分成交（GTC订单的情况）
    /// 对冲策略已暂时关闭，部分成交不平衡不做任何处理
    pub async fn handle_partial_fill(
        &self,
        pair: &OrderPair,
        _position_tracker: &PositionTracker,
    ) -> Result<RecoveryAction> {
        // 计算不平衡数量
        let imbalance = (pair.yes_filled - pair.no_filled).abs();
        let total_filled = pair.yes_filled + pair.no_filled;

        // 计算不平衡比例
        let imbalance_ratio = if total_filled > dec!(0) {
            imbalance / total_filled
        } else {
            dec!(0)
        };

        // 对冲策略已关闭，部分成交不平衡不做任何处理
        if imbalance_ratio > self.imbalance_threshold {
            let (side, amount) = if pair.yes_filled > pair.no_filled {
                // YES成交多
                ("YES", pair.yes_filled - pair.no_filled)
            } else {
                // NO成交多
                ("NO", pair.no_filled - pair.yes_filled)
            };

            debug!(
                pair_id = %pair.pair_id,
                side = side,
                imbalance_amount = %amount,
                imbalance_ratio = %imbalance_ratio,
                "部分成交不平衡，对冲已关，不处理"
            );
        }

        // 返回None，不做任何对冲处理
        Ok(RecoveryAction::None)
        
        // 旧代码：如果不平衡超过阈值，需要对冲
        // if imbalance_ratio > self.imbalance_threshold {
        //     let (token_to_sell, amount) = if pair.yes_filled > pair.no_filled {
        //         // YES成交多，卖出多余的YES
        //         (pair.yes_token_id, pair.yes_filled - pair.no_filled)
        //     } else {
        //         // NO成交多，卖出多余的NO
        //         (pair.no_token_id, pair.no_filled - pair.yes_filled)
        //     };
        // 
        //     info!(
        //         pair_id = %pair.pair_id,
        //         token_id = %token_to_sell,
        //         amount = %amount,
        //         imbalance_ratio = %imbalance_ratio,
        //         "部分成交不平衡，执行对冲"
        //     );
        // 
        //     return Ok(RecoveryAction::SellExcess {
        //         token_id: token_to_sell.to_string(),
        //         amount,
        //     });
        // }
        // 
        // // 不平衡在可接受范围内
        // Ok(RecoveryAction::None)
    }

    /// 处理只购买一边成功（GTC订单的情况）
    /// 对冲策略已暂时关闭，单边成交不做任何处理
    pub async fn handle_one_sided_fill(
        &self,
        pair: &OrderPair,
        _position_tracker: &PositionTracker,
    ) -> Result<RecoveryAction> {
        // 确定哪个订单成功，哪个失败
        let (side, filled_amount) =
            if pair.yes_filled > dec!(0) && pair.no_filled == dec!(0) {
                // YES成功，NO失败（可能还在挂单）
                ("YES", pair.yes_filled)
            } else if pair.no_filled > dec!(0) && pair.yes_filled == dec!(0) {
                // NO成功，YES失败（可能还在挂单）
                ("NO", pair.no_filled)
            } else {
                return Ok(RecoveryAction::None);
            };

        // 对冲策略已关闭，单边成交不做任何处理（详情由 executor 的 ⚠️ 单边成交 已记录）
        debug!(
            "单边成交 | {} 成交 {} 份 | 对冲已关，不处理",
            side, filled_amount
        );

        // 返回None，不做任何对冲处理
        Ok(RecoveryAction::None)
        
        // 旧代码：对冲策略：监测买一价，达到止盈止损时卖出
        // // 确定对立边的token_id
        // let success_token = if pair.yes_filled > dec!(0) {
        //     pair.yes_token_id
        // } else {
        //     pair.no_token_id
        // };
        // let opposite_token = if success_token == pair.yes_token_id {
        //     pair.no_token_id
        // } else {
        //     pair.yes_token_id
        // };
        // 
        // Ok(RecoveryAction::MonitorForExit {
        //     token_id: success_token,
        //     opposite_token_id: opposite_token,
        //     amount: filled_amount,
        //     entry_price: dec!(0), // 占位符，需要在主程序中从订单簿获取
        //     take_profit_pct: self.take_profit_pct,
        //     stop_loss_pct: self.stop_loss_pct,
        //     pair_id: pair.pair_id.clone(),
        //     market_display: "未知市场".to_string(), // 占位符，需要在主程序中从市场信息获取
        // })
    }
}

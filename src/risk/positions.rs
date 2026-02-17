use anyhow::Result;
use dashmap::DashMap;
use polymarket_client_sdk::types::{Decimal, U256};
use rust_decimal_macros::dec;
use tracing::{debug, info, trace};

use poly_5min_bot::positions::{get_positions, Position};

pub struct PositionTracker {
    positions: DashMap<U256, Decimal>, // token_id -> 数量（正数=持有多头，负数=持有空头）
    exposure_costs: DashMap<U256, Decimal>, // token_id -> 成本（USD），用于跟踪风险敞口
    max_exposure: Decimal,
}

impl PositionTracker {
    pub fn new(max_exposure: Decimal) -> Self {
        Self {
            positions: DashMap::new(),
            exposure_costs: DashMap::new(),
            max_exposure,
        }
    }

    pub fn update_position(&self, token_id: U256, delta: Decimal) {
        trace!("update_position: 开始 | token_id:{} | delta:{}", token_id, delta);
        
        trace!("update_position: 准备获取positions写锁");
        let mut entry = self.positions.entry(token_id).or_insert(dec!(0));
        trace!("update_position: positions写锁已获取");
        *entry += delta;
        trace!("update_position: 持仓已更新，新值:{}", *entry);

        // 如果持仓变为0或接近0，可以清理
        // 关键修复：先释放 positions 的写锁，再访问 exposure_costs
        // 这样可以避免与 update_exposure_cost 的死锁
        let should_remove = entry.abs() < dec!(0.0001);
        trace!("update_position: should_remove:{}", should_remove);
        if should_remove {
            *entry = dec!(0);
            trace!("update_position: 持仓已清零");
        }
        // 释放 positions 的锁
        drop(entry);
        trace!("update_position: positions写锁已释放");
        
        // 现在可以安全地访问 exposure_costs
        if should_remove {
            trace!("update_position: 准备remove exposure_costs");
            self.exposure_costs.remove(&token_id);
            trace!("update_position: exposure_costs已remove");
        }
        
        trace!("update_position: 完成");
    }

    /// 更新风险敞口成本（USD）
    /// price: 买入价格
    /// delta: 持仓变化量（正数=买入，负数=卖出）
    pub fn update_exposure_cost(&self, token_id: U256, price: Decimal, delta: Decimal) {
        trace!("update_exposure_cost: 开始 | token_id:{} | price:{} | delta:{}", token_id, price, delta);
        
        if delta == dec!(0) {
            trace!("update_exposure_cost: delta为0，直接返回");
            return; // 没有变化，不需要更新
        }
        
        trace!("update_exposure_cost: 准备获取positions读锁");
        // 关键修复：先获取 positions 的读锁，释放后再获取 exposure_costs 的写锁
        // 这样可以避免与 update_position 的死锁（update_position 先获取 positions 写锁，再访问 exposure_costs）
        let current_pos = if delta < dec!(0) {
            trace!("update_exposure_cost: 卖出操作，开始获取positions读锁");
            // 卖出时，需要先获取当前持仓来计算比例
            let pos = self.positions.get(&token_id);
            trace!("update_exposure_cost: positions读锁已获取");
            let result = pos.map(|v| *v.value()).unwrap_or(dec!(0));
            trace!("update_exposure_cost: positions读锁已释放，current_pos:{}", result);
            result
        } else {
            trace!("update_exposure_cost: 买入操作，不需要获取positions");
            dec!(0) // 买入时不需要
        };
        
        trace!("update_exposure_cost: 准备获取exposure_costs写锁");
        // 现在 positions 的锁已经释放，可以安全地获取 exposure_costs 的写锁
        let mut entry = self.exposure_costs.entry(token_id).or_insert(dec!(0));
        trace!("update_exposure_cost: exposure_costs写锁已获取");
        
        if delta > dec!(0) {
            trace!("update_exposure_cost: 买入分支，计算cost_delta");
            // 买入，增加风险敞口（成本 = 价格 * 数量）
            let cost_delta = price * delta;
            *entry += cost_delta;
            trace!("update_exposure_cost: 买入完成，新成本:{}", *entry);
        } else {
            trace!("update_exposure_cost: 卖出分支，current_pos:{}", current_pos);
            // 卖出，减少风险敞口（按比例减少）
            if current_pos > dec!(0) {
                trace!("update_exposure_cost: 计算卖出比例");
                // 计算卖出的比例
                let sell_amount = (-delta).min(current_pos);
                let reduction_ratio = sell_amount / current_pos;
                trace!("update_exposure_cost: sell_amount:{} | reduction_ratio:{} | 当前成本:{}", sell_amount, reduction_ratio, *entry);
                // 按比例减少成本
                *entry = (*entry * (dec!(1) - reduction_ratio)).max(dec!(0));
                trace!("update_exposure_cost: 卖出完成，新成本:{}", *entry);
            } else {
                trace!("update_exposure_cost: current_pos为0，直接清零");
                *entry = dec!(0);
            }
        }
        
        trace!("update_exposure_cost: 检查是否需要清理，当前成本:{}", *entry);
        // 如果成本接近0，清理
        if *entry < dec!(0.01) {
            trace!("update_exposure_cost: 成本接近0，准备清理");
            *entry = dec!(0);
            drop(entry); // 显式释放写锁
            trace!("update_exposure_cost: 写锁已释放，准备remove");
            self.exposure_costs.remove(&token_id);
            trace!("update_exposure_cost: remove完成");
        } else {
            trace!("update_exposure_cost: 成本不为0，保持entry");
            drop(entry); // 显式释放写锁
        }
        
        trace!("update_exposure_cost: 完成");
    }

    /// 获取最大风险敞口限制
    pub fn max_exposure(&self) -> Decimal {
        self.max_exposure
    }

    /// 重置风险敞口（新一轮开始时调用，清空成本缓存，使本轮从 0 敞口重新累计）
    pub fn reset_exposure(&self) {
        self.exposure_costs.clear();
        info!("🔄 风险敞口已重置（新一轮）");
    }

    pub fn get_position(&self, token_id: U256) -> Decimal {
        self.positions
            .get(&token_id)
            .map(|v| *v.value())
            .unwrap_or(dec!(0))
    }

    /// 计算持仓不平衡度（0.0 = 完全平衡，1.0 = 完全不平衡）
    pub fn calculate_imbalance(&self, yes_token: U256, no_token: U256) -> Decimal {
        let yes_pos = self.get_position(yes_token);
        let no_pos = self.get_position(no_token);

        let total = yes_pos + no_pos;
        if total == dec!(0) {
            return dec!(0); // 完全平衡
        }

        // 不平衡度 = abs(yes - no) / (yes + no)
        let imbalance = (yes_pos - no_pos).abs() / total;
        imbalance
    }

    /// 计算当前总风险敞口（USD）
    /// 基于所有持仓的成本总和
    pub fn calculate_exposure(&self) -> Decimal {
        // 计算总风险敞口（所有持仓的成本总和）
        // 使用 collect 先收集到 Vec，避免长时间持有锁
        let costs: Vec<Decimal> = self.exposure_costs
            .iter()
            .map(|entry| *entry.value())
            .collect();
        costs.iter().sum()
    }

    pub fn is_within_limits(&self) -> bool {
        self.calculate_exposure() <= self.max_exposure
    }

    /// 检查如果执行新订单，是否会超过风险敞口限制
    /// yes_cost: YES订单的成本（价格 * 数量）
    /// no_cost: NO订单的成本（价格 * 数量）
    pub fn would_exceed_limit(&self, yes_cost: Decimal, no_cost: Decimal) -> bool {
        let current_exposure = self.calculate_exposure();
        let new_order_cost = yes_cost + no_cost;
        (current_exposure + new_order_cost) > self.max_exposure
    }

    /// 获取YES和NO的持仓
    pub fn get_pair_positions(&self, yes_token: U256, no_token: U256) -> (Decimal, Decimal) {
        (self.get_position(yes_token), self.get_position(no_token))
    }

    /// 从 Data API 同步持仓，完全覆盖本地缓存
    /// 这个方法会从API获取最新持仓，清空并重建本地positions map
    /// 用于定时同步任务，确保本地缓存与链上实际持仓一致
    pub async fn sync_from_api(&self) -> Result<Vec<Position>> {
        use std::collections::HashMap;
        use polymarket_client_sdk::types::B256;
        
        let positions = get_positions().await?;
        
        // 清空现有持仓（敞口仅由「执行套利」时增加、Merge 时扣减，不从 API 回填）
        self.positions.clear();
        
        // 从API获取的持仓更新到本地缓存
        let mut updated_count = 0;
        let mut valid_positions = Vec::new();
        
        for pos in positions {
            if pos.size > dec!(0) {
                // Position.asset 就是 token_id
                self.positions.insert(pos.asset, pos.size);
                valid_positions.push(pos);
                updated_count += 1;
            }
        }
        
        // 按市场分组打印持仓
        if !valid_positions.is_empty() {
            let mut by_market: HashMap<B256, Vec<&Position>> = HashMap::new();
            for pos in &valid_positions {
                by_market.entry(pos.condition_id).or_default().push(pos);
            }
            
            info!("📊 持仓同步完成 | 共 {} 个持仓，{} 个市场", updated_count, by_market.len());
            
            // 按市场分组打印，每个市场一行
            for (_condition_id, market_positions) in by_market.iter() {
                let mut yes_pos = dec!(0);
                let mut no_pos = dec!(0);
                let mut market_title = "";
                
                for pos in market_positions {
                    if pos.outcome_index == 0 {
                        yes_pos = pos.size;
                    } else if pos.outcome_index == 1 {
                        no_pos = pos.size;
                    }
                    if market_title.is_empty() {
                        market_title = &pos.title;
                    }
                }
                
                // 截断过长的标题
                let title_display = if market_title.len() > 40 {
                    format!("{}...", &market_title[..37])
                } else {
                    market_title.to_string()
                };
                
                info!(
                    "  📈 {} | YES:{} NO:{}",
                    title_display,
                    yes_pos,
                    no_pos
                );
            }
        } else {
            info!("📊 持仓同步完成 | 当前无持仓");
        }
        
        Ok(valid_positions)
    }
}

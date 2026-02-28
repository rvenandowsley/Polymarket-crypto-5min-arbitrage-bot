use anyhow::Result;
use alloy::signers::Signer;
use alloy::signers::local::LocalSigner;
use chrono::Utc;
use polymarket_client_sdk::clob::{Client, Config};
use polymarket_client_sdk::clob::types::{OrderType, Side, SignatureType};
use polymarket_client_sdk::types::{Address, Decimal, U256};
use polymarket_client_sdk::POLYGON;
use rust_decimal_macros::dec;
use std::str::FromStr;
use std::time::Instant;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::monitor::arbitrage::ArbitrageOpportunity;

pub struct OrderPairResult {
    pub pair_id: String,
    pub yes_order_id: String,
    pub no_order_id: String,
    pub yes_filled: Decimal,
    pub no_filled: Decimal,
    pub yes_size: Decimal,
    pub no_size: Decimal,
    pub success: bool,
}

pub struct TradingExecutor {
    client: Client<polymarket_client_sdk::auth::state::Authenticated<polymarket_client_sdk::auth::Normal>>,
    private_key: String,
    max_order_size: Decimal,
    slippage: [Decimal; 2], // [first, second]，仅下降侧用 second，上涨与持平用 first
    gtd_expiration_secs: u64,
    arbitrage_order_type: OrderType,
}

impl TradingExecutor {
    pub async fn new(
        private_key: String,
        max_order_size_usdc: f64,
        proxy_address: Option<Address>,
        slippage: [f64; 2],
        gtd_expiration_secs: u64,
        arbitrage_order_type: OrderType,
    ) -> Result<Self> {
        // 验证私钥格式
        let signer = LocalSigner::from_str(&private_key)
            .map_err(|e| anyhow::anyhow!("私钥格式无效: {}. 请确保私钥是64字符的十六进制字符串（不带0x前缀）", e))?
            .with_chain_id(Some(POLYGON));

        let config = Config::builder().use_server_time(false).build();
        let mut auth_builder = Client::new("https://clob.polymarket.com", config)
            .map_err(|e| anyhow::anyhow!("创建CLOB客户端失败: {}", e))?
            .authentication_builder(&signer);
        
        // 如果提供了proxy_address，设置funder和signature_type（按照Python SDK模式）
        if let Some(funder) = proxy_address {
            auth_builder = auth_builder
                .funder(funder)
                .signature_type(SignatureType::Proxy);
        }
        
        let client = auth_builder
            .authenticate()
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "API认证失败: {}. 可能的原因：1) 私钥无效 2) 网络问题 3) Polymarket API服务不可用",
                    e
                )
            })?;

        Ok(Self {
            client,
            private_key,
            max_order_size: Decimal::try_from(max_order_size_usdc)
                .unwrap_or(rust_decimal_macros::dec!(100.0)),
            slippage: [
                Decimal::try_from(slippage[0]).unwrap_or(dec!(0.0)),
                Decimal::try_from(slippage[1]).unwrap_or(dec!(0.01)),
            ],
            gtd_expiration_secs,
            arbitrage_order_type,
        })
    }

    /// 验证认证是否真的成功 - 按照官方示例使用 api_keys() 来验证
    pub async fn verify_authentication(&self) -> Result<()> {
        // 按照官方示例，使用 api_keys() 来验证认证状态
        self.client.api_keys().await
            .map_err(|e| anyhow::anyhow!("认证验证失败: API调用返回错误: {}", e))?;
        Ok(())
    }

    /// 取消该账户所有挂单（收尾时使用）
    pub async fn cancel_all_orders(&self) -> Result<polymarket_client_sdk::clob::types::response::CancelOrdersResponse> {
        self.client
            .cancel_all_orders()
            .await
            .map_err(|e| anyhow::anyhow!("取消所有挂单失败: {}", e))
    }

    /// 以指定价格下 GTC 卖单（收尾时市价意图卖出单腿持仓）
    pub async fn sell_at_price(
        &self,
        token_id: U256,
        price: Decimal,
        size: Decimal,
    ) -> Result<polymarket_client_sdk::clob::types::response::PostOrderResponse> {
        let signer = LocalSigner::from_str(&self.private_key)?
            .with_chain_id(Some(POLYGON));
        let order = self
            .client
            .limit_order()
            .token_id(token_id)
            .side(Side::Sell)
            .price(price)
            .size(size)
            .order_type(OrderType::GTC)
            .build()
            .await?;
        let signed = self.client.sign(&signer, order).await?;
        self.client
            .post_order(signed)
            .await
            .map_err(|e| anyhow::anyhow!("卖出订单提交失败: {}", e))
    }

    /// 按方向取滑点：仅下降(↓)用 second，上涨(↑)和持平(−/空)用 first
    fn slippage_for_direction(&self, dir: &str) -> Decimal {
        if dir == "↓" {
            self.slippage[1]
        } else {
            self.slippage[0]
        }
    }

    /// 执行套利交易（使用post_orders批量提交YES和NO订单；订单类型由 arbitrage_order_type 配置，GTD 时配合 gtd_expiration_secs）
    /// yes_dir / no_dir：涨跌方向 "↑" "↓" "−" 或 ""，用于按方向分配滑点（仅下降=second，上涨与持平=first）
    pub async fn execute_arbitrage_pair(
        &self,
        opp: &ArbitrageOpportunity,
        yes_dir: &str,
        no_dir: &str,
    ) -> Result<OrderPairResult> {
        // 性能计时：总开始时间
        let total_start = Instant::now();
        
        // 这个日志已经在main.rs中打印了，这里不再重复打印
        let expiry_info = if matches!(self.arbitrage_order_type, OrderType::GTD) {
            format!("过期:{}秒", self.gtd_expiration_secs)
        } else {
            "无过期".to_string()
        };
        debug!(
            market_id = %opp.market_id,
            profit_pct = %opp.profit_percentage,
            order_type = %self.arbitrage_order_type,
            "开始执行套利交易（批量下单，订单类型:{}，{}）",
            self.arbitrage_order_type,
            expiry_info
        );

        // 计算实际下单数量（考虑最大订单限制）
        let yes_token_id = U256::from_str(&opp.yes_token_id.to_string())?;
        let no_token_id = U256::from_str(&opp.no_token_id.to_string())?;

        let order_size = opp.yes_size.min(opp.no_size).min(self.max_order_size);

        // 生成订单对ID
        let pair_id = Uuid::new_v4().to_string();

        // 计算过期时间：当前时间 + 配置的过期时间
        let expiration = Utc::now() + chrono::Duration::seconds(self.gtd_expiration_secs as i64);

        // 滑点按涨跌方向分配：上涨=first，下降/持平=second
        let yes_slippage_apply = self.slippage_for_direction(yes_dir);
        let no_slippage_apply = self.slippage_for_direction(no_dir);
        let yes_price_with_slippage = (opp.yes_ask_price + yes_slippage_apply).min(dec!(1.0));
        let no_price_with_slippage = (opp.no_ask_price + no_slippage_apply).min(dec!(1.0));
        
        // 打印选档信息（加滑点后的价格）
        info!(
            "📋 选档 | YES {:.4}×{:.2} NO {:.4}×{:.2}",
            yes_price_with_slippage, order_size,
            no_price_with_slippage, order_size
        );
        
        let expiry_suffix = if matches!(self.arbitrage_order_type, OrderType::GTD) {
            format!(" | GTD {}s", self.gtd_expiration_secs)
        } else {
            String::new()
        };
        info!(
            "📤 下单 | YES {:.4}→{:.4}×{} NO {:.4}→{:.4}×{} | {}{}",
            opp.yes_ask_price, yes_price_with_slippage, order_size,
            opp.no_ask_price, no_price_with_slippage, order_size,
            self.arbitrage_order_type, expiry_suffix
        );

        // 下单前检查：双边金额均须 > $1（交易所最小下单金额）
        let yes_amount_usd = yes_price_with_slippage * order_size;
        let no_amount_usd = no_price_with_slippage * order_size;
        if yes_amount_usd <= dec!(1) || no_amount_usd <= dec!(1) {
            warn!(
                "⏭️ 跳过下单 | YES金额:{:.2} USD NO金额:{:.2} USD | 双边均须 > $1",
                yes_amount_usd, no_amount_usd
            );
            return Err(anyhow::anyhow!(
                "下单金额不满足交易所最小要求: YES {:.2} USD, NO {:.2} USD，双边均须 > $1",
                yes_amount_usd, no_amount_usd
            ));
        }

        // 性能计时：并行构建YES和NO订单开始
        let build_start = Instant::now();
        
        // 并行构建YES和NO订单；仅 GTD 时设置 expiration（SDK 规定非 GTD 不可设过期）
        let (yes_order, no_order) = tokio::join!(
            async {
                let b = self.client
                    .limit_order()
                    .token_id(yes_token_id)
                    .side(Side::Buy)
                    .price(yes_price_with_slippage)
                    .size(order_size)
                    .order_type(self.arbitrage_order_type.clone());
                if matches!(&self.arbitrage_order_type, OrderType::GTD) {
                    b.expiration(expiration).build().await
                } else {
                    b.build().await
                }
            },
            async {
                let b = self.client
                    .limit_order()
                    .token_id(no_token_id)
                    .side(Side::Buy)
                    .price(no_price_with_slippage)
                    .size(order_size)
                    .order_type(self.arbitrage_order_type.clone());
                if matches!(&self.arbitrage_order_type, OrderType::GTD) {
                    b.expiration(expiration).build().await
                } else {
                    b.build().await
                }
            }
        );
        
        let yes_order = yes_order?;
        let no_order = no_order?;
        let build_elapsed = build_start.elapsed().as_millis();

        // 性能计时：并行签名开始
        let sign_start = Instant::now();
        
        // 创建signer
        let signer = LocalSigner::from_str(&self.private_key)?
            .with_chain_id(Some(POLYGON));
        
        // 并行签名YES和NO订单
        let (signed_yes_result, signed_no_result) = tokio::join!(
            self.client.sign(&signer, yes_order),
            self.client.sign(&signer, no_order)
        );
        
        let signed_yes = signed_yes_result?;
        let signed_no = signed_no_result?;
        let sign_elapsed = sign_start.elapsed().as_millis();

        // 性能计时：发送订单开始
        let send_start = Instant::now();
        
        // 单价高的排前面发送；提交后需按相同顺序从 results 中解析 yes_result / no_result
        let yes_first = yes_price_with_slippage >= no_price_with_slippage;
        let orders_to_send: Vec<_> = if yes_first {
            vec![signed_yes, signed_no]
        } else {
            vec![signed_no, signed_yes]
        };
        let results = match self.client.post_orders(orders_to_send).await {
            Ok(results) => {
                let send_elapsed = send_start.elapsed().as_millis();
                let total_elapsed = total_start.elapsed().as_millis();
                
                info!(
                    "⏱️ 耗时 | {} | 构建{}ms 签名{}ms 发送{}ms 总{}ms",
                    &pair_id[..8], build_elapsed, sign_elapsed, send_elapsed, total_elapsed
                );
                
                results
            }
            Err(e) => {
                let send_elapsed = send_start.elapsed().as_millis();
                let total_elapsed = total_start.elapsed().as_millis();
                
                error!(
                    "❌ 批量下单API调用失败 | 订单对ID:{} | YES价格:{} (含滑点) | NO价格:{} (含滑点) | 数量:{} | 构建耗时:{}ms | 签名耗时:{}ms | 发送耗时:{}ms | 总耗时:{}ms | 错误:{}",
                    &pair_id[..8],
                    yes_price_with_slippage,
                    no_price_with_slippage,
                    order_size,
                    build_elapsed,
                    sign_elapsed,
                    send_elapsed,
                    total_elapsed,
                    e
                );
                return Err(anyhow::anyhow!("批量下单API调用失败: {}", e));
            }
        };
        
        // 验证返回结果数量
        if results.len() != 2 {
            error!(
                "❌ 批量下单返回结果数量不正确 | 订单对ID:{} | 期望:2 | 实际:{}",
                &pair_id[..8],
                results.len()
            );
            return Err(anyhow::anyhow!(
                "批量下单返回结果数量不正确 | 期望:2 | 实际:{}",
                results.len()
            ));
        }
        
        // 提取YES和NO订单的结果（提交顺序为单价高者在前，需按 yes_first 映射）
        let (yes_result, no_result) = if yes_first {
            (&results[0], &results[1])
        } else {
            (&results[1], &results[0])
        };

        // 订单返回结果详情已移除，只保留关键信息在后续日志中

        // 检查成交数量（GTD订单的关键指标）
        let yes_filled = yes_result.taking_amount;
        let no_filled = no_result.taking_amount;

        // 对于GTD订单，如果无法在90秒内全部成交，订单会在过期后取消
        // 我们应该检查实际的成交数量，而不是 success 字段
        // 只有在两个订单都完全没有成交时，才返回错误
        if yes_filled == dec!(0) && no_filled == dec!(0) {
            // 提取简化的错误信息
            let yes_error_msg = yes_result
                .error_msg
                .as_deref()
                .unwrap_or("未知错误");
            let no_error_msg = no_result
                .error_msg
                .as_deref()
                .unwrap_or("未知错误");
            
            // 简化错误消息，去掉技术细节
            let yes_error_simple = if yes_error_msg.contains("no orders found to match") {
                "订单簿中无匹配订单"
            } else if yes_error_msg.contains("GTD") || yes_error_msg.contains("FOK") || yes_error_msg.contains("FAK") || yes_error_msg.contains("GTC") {
                "订单无法成交"
            } else {
                yes_error_msg
            };
            
            let no_error_simple = if no_error_msg.contains("no orders found to match") {
                "订单簿中无匹配订单"
            } else if no_error_msg.contains("GTD") || no_error_msg.contains("FOK") || no_error_msg.contains("FAK") || no_error_msg.contains("GTC") {
                "订单无法成交"
            } else {
                no_error_msg
            };

            error!(
                "❌ 套利交易失败 | 订单对ID:{} | YES订单:{} | NO订单:{}",
                &pair_id[..8], // 只显示前8个字符
                yes_error_simple,
                no_error_simple
            );

            // 详细错误信息记录在debug级别
            debug!(
                pair_id = %pair_id,
                yes_order_id = ?yes_result.order_id,
                no_order_id = ?no_result.order_id,
                yes_success = yes_result.success,
                no_success = no_result.success,
                yes_error = %yes_error_msg,
                no_error = %no_error_msg,
                "两个订单都未成交（详细信息）"
            );

            return Err(anyhow::anyhow!(
                "套利失败: YES和NO订单都未成交 | YES: {}, NO: {}",
                yes_error_simple,
                no_error_simple
            ));
        }

        // 如果至少有一个订单成交了，记录警告但不返回错误
        // 让后续的风险管理器来处理单边成交的情况
        if !yes_result.success || !no_result.success {
            let yes_error_msg = yes_result
                .error_msg
                .as_deref()
                .unwrap_or("未知错误");
            let no_error_msg = no_result
                .error_msg
                .as_deref()
                .unwrap_or("未知错误");

            // 简化错误消息
            let yes_error_simple = if yes_error_msg.contains("no orders found to match") {
                "部分未成交（已挂单）"
            } else if yes_error_msg.contains("GTD") || yes_error_msg.contains("FOK") || yes_error_msg.contains("FAK") || yes_error_msg.contains("GTC") {
                "部分未成交（已挂单）"
            } else {
                "状态异常"
            };
            
            let no_error_simple = if no_error_msg.contains("no orders found to match") {
                "部分未成交（已挂单）"
            } else if no_error_msg.contains("GTD") || no_error_msg.contains("FOK") || no_error_msg.contains("FAK") || no_error_msg.contains("GTC") {
                "部分未成交（已挂单）"
            } else {
                "状态异常"
            };

            warn!(
                "⚠️ 部分订单状态异常 | 订单对ID:{} | YES:{} (成交:{}份) | NO:{} (成交:{}份) | 已启动风险管理",
                &pair_id[..8],
                yes_error_simple,
                yes_filled,
                no_error_simple,
                no_filled
            );

            // 详细错误信息记录在debug级别
            debug!(
                pair_id = %pair_id,
                yes_order_id = ?yes_result.order_id,
                no_order_id = ?no_result.order_id,
                yes_success = yes_result.success,
                no_success = no_result.success,
                yes_error = %yes_error_msg,
                no_error = %no_error_msg,
                "订单提交状态异常详情"
            );
        }

        // 根据成交情况打印不同的日志
        if yes_filled > dec!(0) && no_filled > dec!(0) {
            info!(
                "✅ 套利交易成功 | 订单对ID:{} | YES成交:{}份 | NO成交:{}份 | 总成交:{}份",
                &pair_id[..8],
                yes_filled,
                no_filled,
                yes_filled.min(no_filled)
            );
        } else if yes_filled > dec!(0) || no_filled > dec!(0) {
            let side = if yes_filled > dec!(0) { "YES" } else { "NO" };
            let filled = if yes_filled > dec!(0) { yes_filled } else { no_filled };
            let other_side = if yes_filled > dec!(0) { "NO" } else { "YES" };
            warn!(
                "⚠️ 单边成交 | {} | {} 成交 {} 份，{} 未成交（已交风控）",
                &pair_id[..8], side, filled, other_side
            );
        } else {
            warn!(
                "❌ 套利失败 | 订单对ID:{} | YES和NO都未成交",
                &pair_id[..8]
            );
        }

        Ok(OrderPairResult {
            pair_id,
            yes_order_id: yes_result.order_id.clone(),
            no_order_id: no_result.order_id.clone(),
            yes_filled,
            no_filled,
            yes_size: order_size,
            no_size: order_size,
            success: true,
        })
    }
}

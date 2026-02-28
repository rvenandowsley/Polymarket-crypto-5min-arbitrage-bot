use anyhow::Result;
use chrono::{DateTime, Utc};
use std::time::Duration;
use tokio::time::sleep;
use tracing::{error, info, warn};

use super::discoverer::{MarketDiscoverer, MarketInfo};

pub struct MarketScheduler {
    discoverer: MarketDiscoverer,
    refresh_advance_secs: u64,
}

impl MarketScheduler {
    pub fn new(discoverer: MarketDiscoverer, refresh_advance_secs: u64) -> Self {
        Self {
            discoverer,
            refresh_advance_secs,
        }
    }

    /// 计算到下一个5分钟窗口的等待时间
    pub fn calculate_wait_time(&self, now: DateTime<Utc>) -> Duration {
        let next_window_ts = MarketDiscoverer::calculate_next_window_timestamp(now);
        let next_window = DateTime::from_timestamp(next_window_ts, 0)
            .expect("Invalid timestamp");

        // 提前几秒查询，确保市场已创建
        let wait_duration = next_window
            .signed_duration_since(now)
            .to_std()
            .unwrap_or(Duration::ZERO)
            .saturating_sub(Duration::from_secs(self.refresh_advance_secs));

        wait_duration.max(Duration::ZERO)
    }

    /// 立即获取当前窗口的市场，如果失败则等待下一个窗口
    pub async fn get_markets_immediately_or_wait(&self) -> Result<Vec<MarketInfo>> {
        // 首先尝试获取当前窗口的市场
        let now = Utc::now();
        let current_timestamp = MarketDiscoverer::calculate_current_window_timestamp(now);
        let next_timestamp = MarketDiscoverer::calculate_next_window_timestamp(now);

        // 如果当前窗口和下一个窗口相同（理论上 5m 不会发生），走等待逻辑
        if current_timestamp == next_timestamp {
            return self.wait_for_next_window().await;
        }

        info!("尝试获取当前窗口的市场");
        match self.discoverer.get_markets_for_timestamp(current_timestamp).await {
            Ok(markets) => {
                if !markets.is_empty() {
                    info!(count = markets.len(), "发现当前窗口的市场");
                    return Ok(markets);
                }
                // 当前窗口没有市场：可能是新市场尚未创建，先短间隔重试（5m 市场通常几秒内就绪）
                // 若直接调用 wait_for_next_window 会等到下一窗口边界，导致跳过本窗口
                const RETRY_SECS: u64 = 2;
                const MAX_RETRY_SECS: u64 = 90; // 最多重试约 90 秒
                let mut elapsed = 0u64;
                while elapsed < MAX_RETRY_SECS {
                    info!("当前窗口市场为空，{} 秒后重试（已等待 {} 秒）", RETRY_SECS, elapsed);
                    sleep(Duration::from_secs(RETRY_SECS)).await;
                    elapsed += RETRY_SECS;
                    match self.discoverer.get_markets_for_timestamp(current_timestamp).await {
                        Ok(markets) if !markets.is_empty() => {
                            info!(count = markets.len(), "重试成功，发现当前窗口的市场");
                            return Ok(markets);
                        }
                        _ => {}
                    }
                }
                // 重试超时，等待下一窗口
                warn!("重试 {} 秒后仍无市场，等待下一窗口", MAX_RETRY_SECS);
                self.wait_for_next_window().await
            }
            Err(e) => {
                warn!(error = %e, "获取当前窗口市场失败，等待下一个窗口");
                self.wait_for_next_window().await
            }
        }
    }

    /// 等待到下一个5分钟窗口开始，并获取市场
    pub async fn wait_for_next_window(&self) -> Result<Vec<MarketInfo>> {
        loop {
            let wait_time = self.calculate_wait_time(Utc::now());
            if wait_time > Duration::ZERO {
                info!(
                    wait_secs = wait_time.as_secs(),
                    "等待下一个5分钟窗口"
                );
                sleep(wait_time).await;
            }

            // 查询当前窗口的市场
            let now = Utc::now();
            let timestamp = MarketDiscoverer::calculate_current_window_timestamp(now);
            match self.discoverer.get_markets_for_timestamp(timestamp).await {
                Ok(markets) => {
                    if !markets.is_empty() {
                        info!(count = markets.len(), "发现新市场");
                        return Ok(markets);
                    }
                    // 如果市场还未创建，等待一段时间后重试
                    info!("市场尚未创建，等待重试...");
                    sleep(Duration::from_secs(2)).await;
                }
                Err(e) => {
                    error!(error = %e, "获取市场失败，重试...");
                    sleep(Duration::from_secs(2)).await;
                }
            }
        }
    }
}

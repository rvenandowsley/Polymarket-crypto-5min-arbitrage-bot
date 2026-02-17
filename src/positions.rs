//! 获取用户当前持仓（Data API）

use anyhow::{Context, Result};
use polymarket_client_sdk::data::types::request::PositionsRequest;
use polymarket_client_sdk::data::Client;
use polymarket_client_sdk::types::Address;

/// Data API 返回的持仓结构，重新导出便于调用方使用
pub use polymarket_client_sdk::data::types::response::Position;

/// 从环境变量 `POLYMARKET_PROXY_ADDRESS` 读取用户地址，调用 Data API 获取当前未平仓持仓。
///
/// # 环境变量
///
/// - `POLYMARKET_PROXY_ADDRESS`: 必填，Polymarket 代理钱包地址（或 EOA 地址）
///
/// # 错误
///
/// - 未设置 `POLYMARKET_PROXY_ADDRESS`
/// - 地址格式无效
/// - 调用 Data API 失败
///
/// # 示例
///
/// ```ignore
/// use poly_15min_bot::positions::{get_positions, Position};
///
/// let positions = get_positions().await?;
/// for p in positions {
///     println!("{}: {} @ {}", p.title, p.size, p.cur_price);
/// }
/// ```
pub async fn get_positions() -> Result<Vec<Position>> {
    dotenvy::dotenv().ok();
    let addr = std::env::var("POLYMARKET_PROXY_ADDRESS")
        .context("POLYMARKET_PROXY_ADDRESS 未设置")?;
    let user: Address = addr
        .parse()
        .context("POLYMARKET_PROXY_ADDRESS 格式无效")?;
    let client = Client::default();
    let req = PositionsRequest::builder().user(user).build();
    client.positions(&req).await.context("获取持仓失败")
}

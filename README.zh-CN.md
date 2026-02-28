# poly_5min_bot

[English](README.md) | **中文**

程序已经是永久使用了，license.key就是永久的授权，不要再找我要授权凭证了。

#### 5Min： https://github.com/rvenandowsley/Polymarket-crypto-5min-arbitrage-bot
#### 15Min： https://github.com/rvenandowsley/Polymarket-crypto-15min-arbitrage-bot
#### 1Hour： https://github.com/rvenandowsley/Polymarket-crypto-1hour-arbitrage-bot

面向 [Polymarket](https://polymarket.com) 加密货币「涨跌」5 分钟市场（UTC 时间）的 Rust 套利机器人。监控订单簿、检测 YES+NO 价差套利机会、通过 CLOB API 下单，并可定时对可赎回持仓执行 merge。

## 功能

- **市场发现**：按币种与 5 分钟时间窗口，从 Gamma API 拉取「涨/跌」5 分钟市场（如 `btc-updown-5m-1770972300`）。
- **订单簿监控**：订阅 CLOB 订单簿，在 `yes_ask + no_ask < 1` 时判定套利机会。
- **套利执行**：下 YES、NO 双单（GTC/GTD/FOK/FAK），可配置滑点、单笔上限与执行价差。
- **风险管理**：跟踪敞口、遵守 `RISK_MAX_EXPOSURE_USDC`，可选对冲监控（当前对冲逻辑已关闭）。
- **Merge 任务**：定时拉取持仓，对 YES、NO 双边都持仓的市场执行 `merge_max` 赎回（需配置 `POLYMARKET_PROXY_ADDRESS` 与 `MERGE_INTERVAL_MINUTES`）。

---
### TG联系方式：[@polyboy123](https://t.me/polyboy123)

<img width="1027" height="788" alt="image" src="https://github.com/user-attachments/assets/7ea3f755-5afa-4e4c-939d-6532e76cdac0" />

---

## 使用已经编译好的包
### 暂时只支持Linux 和 windows
1. 从发布页面下载编译好的程序包：poly_5min_bot.zip
2. 放到云服务器上面，需要确保所在地域能够被polymarket允许交易
3. 配置好.env中前面的几个空白参数，参数由polymarket官网导出
4. linux运行：`./poly_5min_bot`
5. windows运行 `poly_5min_bot.exe`

## 快速开始

```bash
# 1. 安装 Rust（如未安装）、克隆项目、进入目录
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
git clone https://github.com/rvenandowsley/Polymarket-crypto-5min-arbitrage-bot.git && cd Polymarket-crypto-5min-arbitrage-bot

# 2. 复制并编辑 .env
cp .env.example .env
# 编辑 .env：设置 POLYMARKET_PRIVATE_KEY（必填）

# 3. 构建并运行
cargo build --release && cargo run --release
```

---

## 安装步骤

### 1. 安装 Rust

若未安装 Rust：

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
rustc --version   # 确认版本 ≥ 1.70
```

### 2. 获取项目

```bash
git clone https://github.com/rvenandowsley/Polymarket-crypto-5min-arbitrage-bot.git
cd Polymarket-crypto-5min-arbitrage-bot
```

或下载并解压项目压缩包。

### 3. 配置环境变量

```bash
cp .env.example .env
# 编辑 .env，填写必填变量（见下方配置说明）
```

### 4. 构建

```bash
cargo build --release
```

---

## 配置说明

在项目根目录创建 `.env`（可复制 `.env.example`）。环境变量说明：

| 变量名 | 必填 | 说明 |
|--------|------|------|
| `POLYMARKET_PRIVATE_KEY` | 是 | 64 位十六进制私钥（不带 `0x`）。可从 [reveal.magic.link/polymarket](https://reveal.magic.link/polymarket) 导出。 |
| `POLYMARKET_PROXY_ADDRESS` | 否* | 代理钱包地址（Email/Magic 或 Browser Wallet）。启用 merge 任务时必填。 |
| `POLY_BUILDER_API_KEY` | 否* | Builder API Key（Polymarket 设置中获取）。Merge 功能需要。 |
| `POLY_BUILDER_SECRET` | 否* | Builder API Secret。Merge 功能需要。 |
| `POLY_BUILDER_PASSPHRASE` | 否* | Builder API Passphrase。Merge 功能需要。 |
| `MIN_PROFIT_THRESHOLD` | 否 | 套利检测最低利润率，默认 `0.001`。 |
| `MAX_ORDER_SIZE_USDC` | 否 | 单笔最大下单量（USDC），默认 `100.0`。 |
| `CRYPTO_SYMBOLS` | 否 | 币种列表，逗号分隔，如 `bitcoin,ethereum,solana,xrp`，默认 `bitcoin,ethereum,solana,xrp`。 |
| `MARKET_REFRESH_ADVANCE_SECS` | 否 | 提前多少秒刷新下一窗口市场，默认 `5`。 |
| `RISK_MAX_EXPOSURE_USDC` | 否 | 最大敞口上限（USDC），默认 `1000.0`。 |
| `RISK_IMBALANCE_THRESHOLD` | 否 | 风险不平衡阈值，默认 `0.1`。 |
| `HEDGE_TAKE_PROFIT_PCT` | 否 | 对冲止盈百分比，默认 `0.05`。 |
| `HEDGE_STOP_LOSS_PCT` | 否 | 对冲止损百分比，默认 `0.05`。 |
| `ARBITRAGE_EXECUTION_SPREAD` | 否 | 当 `yes+no <= 1 - spread` 时执行套利，默认 `0.01`。 |
| `SLIPPAGE` | 否 | `"first,second"` 或单个值，默认 `0,0.01`。 |
| `GTD_EXPIRATION_SECS` | 否 | GTD 订单过期时间（秒），默认 `300`。 |
| `ARBITRAGE_ORDER_TYPE` | 否 | `GTC` / `GTD` / `FOK` / `FAK`，默认 `GTD`。 |
| `STOP_ARBITRAGE_BEFORE_END_MINUTES` | 否 | 市场结束前 N 分钟停止套利；`0` 表示不限制，默认 `0`。 |
| `MERGE_INTERVAL_MINUTES` | 否 | Merge 执行间隔（分钟）；`0` 表示不启用，默认 `0`。 |
| `MIN_YES_PRICE_THRESHOLD` | 否 | 仅当 YES 价格 ≥ 此值时才套利；`0` 表示不限制，默认 `0`。 |
| `MIN_NO_PRICE_THRESHOLD` | 否 | 仅当 NO 价格 ≥ 此值时才套利；`0` 表示不限制，默认 `0`。 |
| `POLY_15MIN_BOT_LICENSE` | 否 | 自定义许可证文件路径；默认 `./license.key`。 |

---

## 构建与运行

完成 [安装步骤](#安装步骤) 后：

```bash
# 构建 release 版本
cargo build --release

# 运行机器人
cargo run --release
```

或直接运行已构建的二进制：

```bash
./target/release/poly_15min_bot
```

**日志**：在 `.env` 中设置 `RUST_LOG`，或在运行前设置（如 `RUST_LOG=info` 或 `RUST_LOG=debug`）。

**后台运行**（Linux/macOS）：

```bash
nohup ./target/release/poly_15min_bot > bot.log 2>&1 &
```

**后台运行**（Windows）：

```bash
poly_5min_bot.exe
```
---

## 测试用二进制

| 二进制 | 用途 |
|--------|------|
| `test_merge` | 对指定市场执行 merge；需 `POLYMARKET_PRIVATE_KEY`、`POLYMARKET_PROXY_ADDRESS`。 |
| `test_order` | 测试下单。 |
| `test_positions` | 拉取持仓；需 `POLYMARKET_PROXY_ADDRESS`。 |
| `test_price` | 价格/订单簿相关测试。 |
| `test_trade` | 交易执行测试。 |

运行示例：

```bash
cargo run --release --bin test_merge
cargo run --release --bin test_positions
# 其他同理
```

---

## 项目结构

```
src/
├── main.rs           # 入口、merge 任务、主循环（订单簿 + 套利）
├── config.rs         # 从环境变量加载配置
├── lib.rs            # 库入口（merge、positions）
├── merge.rs          # Merge 逻辑
├── positions.rs      # 持仓拉取
├── market/           # 市场发现、调度
├── monitor/          # 订单簿、套利检测
├── risk/             # 风险管理、对冲监控、恢复
├── trading/          # 执行器、订单
└── bin/              # test_merge、test_order、test_positions 等
```

---

## 免责声明

本机器人对接真实市场与资金，请自行承担使用风险。使用前请充分理解配置项、风险限额及 Polymarket 相关条款。

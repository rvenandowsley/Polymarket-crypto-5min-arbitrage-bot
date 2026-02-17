//! 许可证生成工具（仅作者使用）：根据过期时间生成 license.key 内容。
//!
//! 用法示例：
//!   cargo run --bin gen_license -- --hours 24
//!   cargo run --bin gen_license -- --until "2025-02-03 00:00:00"
//!   cargo run --bin gen_license -- --hours 24 --out license.key

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    let mut hours: Option<u64> = None;
    let mut until: Option<String> = None;
    let mut out_path: Option<PathBuf> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--hours" => {
                i += 1;
                hours = Some(
                    args.get(i)
                        .context("--hours 需要参数")?
                        .parse()
                        .context("--hours 必须为正整数")?,
                );
                i += 1;
            }
            "--until" => {
                i += 1;
                until = Some(
                    args.get(i)
                        .context("--until 需要参数（如 2025-02-03 00:00:00）")?
                        .clone(),
                );
                i += 1;
            }
            "--out" => {
                i += 1;
                out_path = Some(
                    args.get(i)
                        .context("--out 需要参数")?
                        .into(),
                );
                i += 1;
            }
            _ => {
                eprintln!("用法: gen_license --hours <N> | --until \"<datetime>\" [--out license.key]");
                eprintln!("  --hours N    从当前起 N 小时后过期");
                eprintln!("  --until \"...\" 指定过期时间（UTC），格式如 2025-02-03 00:00:00");
                eprintln!("  --out FILE   写入文件，不指定则输出到 stdout");
                std::process::exit(1);
            }
        }
    }

    let expiry_secs: u64 = if let Some(h) = hours {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        now + h * 3600
    } else if let Some(ref dt_str) = until {
        let dt_utc: DateTime<Utc> = DateTime::parse_from_rfc3339(dt_str)
            .map(|d| d.with_timezone(&Utc))
            .or_else(|_| {
                chrono::NaiveDateTime::parse_from_str(dt_str, "%Y-%m-%d %H:%M:%S")
                    .map(|n| n.and_utc())
            })
            .context("解析 --until 时间失败，请使用 2025-02-03 00:00:00 或 RFC3339 格式")?;
        dt_utc.timestamp() as u64
    } else {
        anyhow::bail!("请指定 --hours <N> 或 --until \"<datetime>\"");
    };

    let license = poly_5min_bot::trial::create_license(expiry_secs)?;

    if let Some(path) = out_path {
        fs::write(&path, &license).context("写入许可证文件失败")?;
        eprintln!("已写入: {}", path.display());
    } else {
        io::stdout().write_all(license.as_bytes())?;
        io::stdout().flush()?;
    }
    Ok(())
}

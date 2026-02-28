//! 许可证文件授权：程序仅在有有效许可证时运行。
//! 许可证由作者签发，内容为加密的过期时间戳，删除许可证将无法运行，无法通过删文件重置试用。

use aes_gcm::{
    aead::{Aead, AeadCore, KeyInit, OsRng},
    Aes256Gcm, Nonce,
};
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// 默认许可证文件名（放在程序当前工作目录或由环境变量指定路径）
const LICENSE_FILENAME: &str = "license.key";

/// 环境变量：许可证文件路径（可选），未设置时使用当前目录下的 license.key
const LICENSE_PATH_ENV: &str = "POLY_15MIN_BOT_LICENSE";

/// 密钥派生种子（仅用于派生加密密钥；生成许可证时使用相同种子）
const TRIAL_KEY_SEED: &[u8] = b"poly_15min_bot_trial_seed_2025";

/// AES-GCM nonce 长度（12 字节）
const NONCE_LEN: usize = 12;

/// 解析许可证文件路径：优先使用环境变量，否则为当前目录下的 license.key
fn license_file_path() -> PathBuf {
    std::env::var(LICENSE_PATH_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(LICENSE_FILENAME))
}

/// 从种子派生 256 位密钥（SHA-256）
fn derive_key() -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(TRIAL_KEY_SEED);
    let digest = hasher.finalize();
    digest.into()
}

/// 当前时间的 Unix 时间戳（秒）
fn now_secs() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .context("系统时间异常")
}

/// 加密一个 u64 时间戳：输出 base64(nonce || ciphertext)，密文含认证标签防篡改。
fn encrypt_timestamp(ts_secs: u64) -> Result<String> {
    let key = derive_key();
    let cipher = Aes256Gcm::new_from_slice(&key).context("初始化加密失败")?;
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let plaintext = ts_secs.to_le_bytes();
    let ciphertext = cipher
        .encrypt(&nonce, plaintext.as_ref())
        .map_err(|e| anyhow::anyhow!("加密失败: {}", e))?;
    let mut payload = nonce.to_vec();
    payload.extend_from_slice(&ciphertext);
    Ok(base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        &payload,
    ))
}

/// 解密许可证/试用状态内容，返回 u64 时间戳；解密失败或篡改则返回错误。
fn decrypt_timestamp(encoded: &str) -> Result<u64> {
    let payload = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        encoded.trim(),
    )
    .context("许可证格式无效（base64 解码失败）")?;
    if payload.len() < NONCE_LEN {
        anyhow::bail!("许可证无效或已篡改（数据过短）");
    }
    let (nonce_bytes, ciphertext) = payload.split_at(NONCE_LEN);
    let nonce_arr: [u8; NONCE_LEN] = nonce_bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("许可证无效或已篡改（nonce 长度异常）"))?;
    let nonce = Nonce::from(nonce_arr);
    let key = derive_key();
    let cipher = Aes256Gcm::new_from_slice(&key).context("初始化解密失败")?;
    let plaintext = cipher
        .decrypt(&nonce, ciphertext)
        .map_err(|_| anyhow::anyhow!("许可证无效或已篡改（解密或校验失败）"))?;
    if plaintext.len() != 8 {
        anyhow::bail!("许可证无效或已篡改（内容长度异常）");
    }
    let mut bytes: [u8; 8] = [0; 8];
    bytes.copy_from_slice(&plaintext[..8]);
    Ok(u64::from_le_bytes(bytes))
}

/// 生成许可证字符串（过期时间戳加密后的 base64）。
/// 供作者使用：用 `gen_license` 二进制或调用此函数生成许可证，将结果写入文件发给试用用户。
pub fn create_license(expiry_secs: u64) -> Result<String> {
    encrypt_timestamp(expiry_secs)
}

/// 校验许可证文件：文件必须存在且未过期，否则返回错误。
/// 删除许可证将无法运行，无法通过删文件重置试用。
pub fn check_license() -> Result<()> {
    let path = license_file_path();
    let now = now_secs()?;

    if !path.exists() {
        anyhow::bail!(
            "未找到许可证文件。请将作者提供的 {} 放在程序运行目录，或设置环境变量 {} 指定路径。",
            LICENSE_FILENAME,
            LICENSE_PATH_ENV
        );
    }

    let content = fs::read_to_string(&path).context("读取许可证文件失败")?;
    let expiry_secs = decrypt_timestamp(&content)?;

    if now >= expiry_secs {
        anyhow::bail!(
            "许可证已过期。如需继续使用请联系作者获取新许可证。"
        );
    }

    let remaining_secs = expiry_secs - now;
    tracing::info!(
        remaining_hours = (remaining_secs as f64) / 3600.0,
        "许可证有效，剩余约 {:.1} 小时",
        (remaining_secs as f64) / 3600.0
    );
    Ok(())
}

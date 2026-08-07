//! 分级诊断日志（轻量门面）：DEBUG < INFO < WARN < ERROR < CRITICAL。
//!
//! crate 只提供门面与级别；落盘/轮转/脱敏策略由使用方实现（可参考 mistake-agent
//! 的 flexi_logger 集成）。敏感值统一经 [`redact_secret`] 脱敏。

use std::sync::Arc;

/// 诊断日志级别：DEBUG < INFO < WARN < ERROR < CRITICAL。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize, serde::Serialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
    Critical,
}

/// 日志门面：委托 `log` crate。
#[derive(Debug, Clone, Default)]
pub struct Logger;

impl Logger {
    pub fn log(&self, level: Level, message: &str) {
        match level {
            Level::Debug => log::debug!("{message}"),
            Level::Info => log::info!("{message}"),
            Level::Warn => log::warn!("{message}"),
            Level::Error => log::error!("{message}"),
            // CRITICAL 映射到 ERROR 级 + 标记（PANIC 由使用方 hook 承担）。
            Level::Critical => log::error!("[CRITICAL] {message}"),
        }
    }

    pub fn debug(&self, m: &str) {
        self.log(Level::Debug, m);
    }
    pub fn info(&self, m: &str) {
        self.log(Level::Info, m);
    }
    pub fn warn(&self, m: &str) {
        self.log(Level::Warn, m);
    }
    pub fn error(&self, m: &str) {
        self.log(Level::Error, m);
    }
    pub fn critical(&self, m: &str) {
        self.log(Level::Critical, m);
    }
    /// panic hook 用：CRITICAL + [PANIC] 标记。
    pub fn panic(&self, m: &str) {
        log::error!("[PANIC] {m}");
    }
}

pub type LoggerHandle = Arc<Logger>;

/// 敏感值脱敏：API key、令牌等一律脱敏。
pub fn redact_secret(value: &str) -> String {
    const MASK: &str = "****";
    if value.len() <= 8 {
        MASK.to_string()
    } else {
        let head: String = value.chars().take(4).collect();
        let tail: String = value
            .chars()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("{head}{MASK}{tail}")
    }
}

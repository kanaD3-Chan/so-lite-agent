//! storage 内核插件（Linux 内核组织风格）：JSONL 会话事实日志落盘。
//!
//! 插件信息：
//! - namespace `storage`（provides ServiceId::custom("storage")）；
//! - 能力：会话事件日志持久化（[`JsonlSessionStore`]，ADR-0007 第二步）；
//! - 纯服务提供者：无工具/命令/事件入口，`register` 为空（服务实例由
//!   KernelBuilder 引导装配——数据根目录由使用方指定，参考 mistake-agent
//!   StoragePlugin 的"服务实例由 Kernel::new 引导构造"约定）。

use crate::contract::{Info, PluginError};
use crate::context::KernelContext;
use crate::registry::{KernelDescriptor, KernelPlugin};
use crate::services::{JsonlSessionStore, ServiceId};

pub struct StoragePlugin;

impl KernelPlugin for StoragePlugin {
    fn info() -> Info {
        Info {
            namespace: "storage".into(),
            provides: vec![ServiceId::custom("storage")],
            ..Default::default()
        }
    }

    fn register(_ctx: KernelContext<'_>) -> Result<(), PluginError> {
        Ok(())
    }
}

pub fn descriptor() -> KernelDescriptor {
    KernelDescriptor::from_plugin::<StoragePlugin>()
}

/// 内置 JSONL 会话存储（`sl-agent` 默认会话持久化，ADR-0007 第二步）。
pub type DefaultSessionStore = JsonlSessionStore;

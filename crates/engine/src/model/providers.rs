//! Provider 注册表：使用方注册具名模型服务，不做全局可变状态。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use super::contract::ModelService;

/// Provider 注册表：使用方注册具名模型服务，供 KernelBuilder / 插件按名取用。
/// 不做全局可变状态，实例由使用方持有。
#[derive(Default, Clone)]
pub struct ProviderRegistry {
    providers: Arc<RwLock<HashMap<String, Arc<dyn ModelService>>>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册具名 provider；重名拒绝（fail-fast）。
    pub fn register(
        &self,
        name: impl Into<String>,
        provider: Arc<dyn ModelService>,
    ) -> Result<(), String> {
        let name = name.into();
        let mut providers = self.providers.write().expect("registry poisoned");
        if providers.contains_key(&name) {
            return Err(format!("provider 已存在：{name}"));
        }
        providers.insert(name, provider);
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn ModelService>> {
        self.providers
            .read()
            .expect("registry poisoned")
            .get(name)
            .cloned()
    }

    pub fn names(&self) -> Vec<String> {
        self.providers
            .read()
            .expect("registry poisoned")
            .keys()
            .cloned()
            .collect()
    }
}

/// 便捷函数：等价 `registry.register(name, provider)`。
pub fn register_provider(
    registry: &ProviderRegistry,
    name: &str,
    provider: Arc<dyn ModelService>,
) -> Result<(), String> {
    registry.register(name, provider)
}

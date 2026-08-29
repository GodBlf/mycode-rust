use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, RwLock},
};

use thiserror::Error;

use crate::tool::Tool;

#[derive(Debug, Error)]
pub enum ToolRegistryError {
    #[error("a tool named {name} is already registered")]
    Duplicate { name: String },
}

#[derive(Default)]
pub struct ToolRegistry {
    tools: RwLock<BTreeMap<String, Arc<dyn Tool>>>,
    discovered: Mutex<BTreeSet<String>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, tool: impl Tool + 'static) -> Result<(), ToolRegistryError> {
        let tool: Arc<dyn Tool> = Arc::new(tool);
        let name = tool.name().to_string();
        let mut tools = self
            .tools
            .write()
            .expect("tool registry lock should not be poisoned");
        if tools.contains_key(&name) {
            return Err(ToolRegistryError::Duplicate { name });
        }
        tools.insert(name, tool);
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools
            .read()
            .expect("tool registry lock should not be poisoned")
            .get(name)
            .cloned()
    }

    pub fn list(&self) -> Vec<Arc<dyn Tool>> {
        self.tools
            .read()
            .expect("tool registry lock should not be poisoned")
            .values()
            .cloned()
            .collect()
    }

    pub fn schemas(&self) -> Vec<serde_json::Value> {
        let tools = self
            .tools
            .read()
            .expect("tool registry lock should not be poisoned");
        let discovered = self
            .discovered
            .lock()
            .expect("tool discovery lock should not be poisoned");
        tools
            .values()
            .filter(|tool| !tool.is_deferred() || discovered.contains(tool.name()))
            .map(|tool| tool.schema())
            .collect()
    }

    pub fn deferred_names(&self) -> Vec<String> {
        let tools = self
            .tools
            .read()
            .expect("tool registry lock should not be poisoned");
        let discovered = self
            .discovered
            .lock()
            .expect("tool discovery lock should not be poisoned");
        tools
            .values()
            .filter(|tool| tool.is_deferred() && !discovered.contains(tool.name()))
            .map(|tool| tool.name().to_string())
            .collect()
    }

    pub fn mark_discovered(&self, name: &str) {
        if self.get(name).is_some() {
            let mut discovered = self
                .discovered
                .lock()
                .expect("tool discovery lock should not be poisoned");
            discovered.insert(name.to_string());
        }
    }

    pub fn search_deferred(&self, query: &str, max_results: usize) -> Vec<Arc<dyn Tool>> {
        let query = query.to_lowercase();
        self.tools
            .read()
            .expect("tool registry lock should not be poisoned")
            .values()
            .filter(|tool| {
                tool.is_deferred()
                    && (tool.name().to_lowercase().contains(&query)
                        || tool.description().to_lowercase().contains(&query))
            })
            .take(max_results)
            .cloned()
            .collect()
    }

    pub fn find_deferred_by_names(&self, names: &[&str]) -> Vec<Arc<dyn Tool>> {
        let names: BTreeSet<_> = names.iter().map(|name| name.to_lowercase()).collect();
        self.tools
            .read()
            .expect("tool registry lock should not be poisoned")
            .values()
            .filter(|tool| tool.is_deferred() && names.contains(&tool.name().to_lowercase()))
            .cloned()
            .collect()
    }
}

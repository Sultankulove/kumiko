//! Commands service: command registry, context keys, when-expressions (spec, section 11).
//!
//! Every user action is a command. Enablement is decided by the command's when-expression
//! over context keys, evaluated by the core without asking the owner (P6). Key bindings are
//! a separate layer (keymap in `kumiko-config`, P7).

use std::collections::HashMap;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use serde_json::Value;

use kumiko_service_api::BoxFuture;
use kumiko_service_api::when::{Context, WhenExpr};
use kumiko_types::{CommandId, PluginId};

pub type CommandFuture = BoxFuture<'static, Result<Value, CommandError>>;

/// Code behind a command.
pub trait CommandHandler: Send + Sync {
    fn execute(&self, args: Vec<Value>) -> CommandFuture;
}

impl<F> CommandHandler for F
where
    F: Fn(Vec<Value>) -> CommandFuture + Send + Sync,
{
    fn execute(&self, args: Vec<Value>) -> CommandFuture {
        self(args)
    }
}

/// Who registered a command; everything of an owner is released when it goes away.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Owner {
    Core,
    Plugin(PluginId),
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CommandError {
    #[error("command `{0}` is not registered")]
    NotFound(CommandId),
    #[error("command `{0}` is already registered")]
    AlreadyRegistered(CommandId),
    #[error("command `{0}` is disabled in the current context")]
    Disabled(CommandId),
    #[error("command failed: {0}")]
    Failed(String),
}

/// Static description of a command.
#[derive(Debug, Clone)]
pub struct CommandInfo {
    pub id: CommandId,
    /// Title or l10n key (`%command.save%`).
    pub title: Option<String>,
    pub owner: Owner,
    pub when: Option<WhenExpr>,
}

struct Registered {
    info: CommandInfo,
    handler: Arc<dyn CommandHandler>,
}

/// Context keys: the state when-expressions are evaluated against.
#[derive(Default)]
pub struct ContextKeys {
    values: RwLock<HashMap<String, Value>>,
}

fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl ContextKeys {
    /// Sets a key; `null` removes it.
    pub fn set(&self, key: impl Into<String>, value: Value) {
        let key = key.into();
        let mut values = write(&self.values);
        if value.is_null() {
            values.remove(&key);
        } else {
            values.insert(key, value);
        }
    }

    pub fn get(&self, key: &str) -> Option<Value> {
        read(&self.values).get(key).cloned()
    }

    /// Removes every key starting with `prefix` (e.g. a plugin's namespace on unload).
    pub fn remove_prefix(&self, prefix: &str) {
        write(&self.values).retain(|key, _| !key.starts_with(prefix));
    }

    pub fn eval(&self, expr: &WhenExpr) -> bool {
        expr.eval(self)
    }
}

impl Context for ContextKeys {
    fn get(&self, key: &str) -> Option<Value> {
        ContextKeys::get(self, key)
    }
}

/// Registry of commands.
#[derive(Default)]
pub struct CommandRegistry {
    commands: RwLock<HashMap<CommandId, Registered>>,
    context: ContextKeys,
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn context(&self) -> &ContextKeys {
        &self.context
    }

    pub fn register(
        &self,
        info: CommandInfo,
        handler: Arc<dyn CommandHandler>,
    ) -> Result<(), CommandError> {
        let mut commands = write(&self.commands);
        if commands.contains_key(&info.id) {
            return Err(CommandError::AlreadyRegistered(info.id));
        }
        commands.insert(info.id.clone(), Registered { info, handler });
        Ok(())
    }

    pub fn unregister(&self, id: &CommandId) -> bool {
        write(&self.commands).remove(id).is_some()
    }

    /// Releases every command of an owner; returns how many were removed.
    pub fn unregister_owner(&self, owner: &Owner) -> usize {
        let mut commands = write(&self.commands);
        let before = commands.len();
        commands.retain(|_, c| &c.info.owner != owner);
        before - commands.len()
    }

    pub fn contains(&self, id: &CommandId) -> bool {
        read(&self.commands).contains_key(id)
    }

    /// Whether the command exists and its when-expression holds.
    pub fn is_enabled(&self, id: &CommandId) -> bool {
        read(&self.commands)
            .get(id)
            .is_some_and(|c| c.info.when.as_ref().is_none_or(|w| self.context.eval(w)))
    }

    /// All commands sorted by id.
    pub fn list(&self) -> Vec<CommandInfo> {
        let mut list: Vec<_> = read(&self.commands)
            .values()
            .map(|c| c.info.clone())
            .collect();
        list.sort_by(|a, b| a.id.cmp(&b.id));
        list
    }

    /// Runs a command if it is enabled.
    pub async fn execute(&self, id: &CommandId, args: Vec<Value>) -> Result<Value, CommandError> {
        let handler = {
            let commands = read(&self.commands);
            let command = commands
                .get(id)
                .ok_or_else(|| CommandError::NotFound(id.clone()))?;
            if let Some(when) = &command.info.when
                && !self.context.eval(when)
            {
                return Err(CommandError::Disabled(id.clone()));
            }
            command.handler.clone()
        };
        handler.execute(args).await
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn id(s: &str) -> CommandId {
        CommandId::new(s).unwrap()
    }

    fn info(name: &str, owner: Owner, when: Option<&str>) -> CommandInfo {
        CommandInfo {
            id: id(name),
            title: None,
            owner,
            when: when.map(|w| WhenExpr::parse(w).unwrap()),
        }
    }

    fn echo() -> Arc<dyn CommandHandler> {
        Arc::new(|args: Vec<Value>| -> CommandFuture { Box::pin(async move { Ok(json!(args)) }) })
    }

    #[tokio::test]
    async fn register_and_execute() {
        let registry = CommandRegistry::new();
        registry
            .register(info("file.save", Owner::Core, None), echo())
            .unwrap();
        let result = registry.execute(&id("file.save"), vec![json!(1)]).await;
        assert_eq!(result.unwrap(), json!([1]));
        assert_eq!(
            registry.register(info("file.save", Owner::Core, None), echo()),
            Err(CommandError::AlreadyRegistered(id("file.save")))
        );
        assert_eq!(
            registry.execute(&id("file.nope"), vec![]).await,
            Err(CommandError::NotFound(id("file.nope")))
        );
    }

    #[tokio::test]
    async fn when_expression_gates_execution() {
        let registry = CommandRegistry::new();
        registry
            .register(
                info(
                    "editor.format",
                    Owner::Core,
                    Some("editorFocus && !readOnly"),
                ),
                echo(),
            )
            .unwrap();
        let format = id("editor.format");
        assert!(!registry.is_enabled(&format));
        assert_eq!(
            registry.execute(&format, vec![]).await,
            Err(CommandError::Disabled(format.clone()))
        );
        registry.context().set("editorFocus", json!(true));
        assert!(registry.is_enabled(&format));
        registry.context().set("readOnly", json!(true));
        assert!(!registry.is_enabled(&format));
        registry.context().set("readOnly", Value::Null);
        assert!(registry.execute(&format, vec![]).await.is_ok());
    }

    #[test]
    fn owners_release_their_commands() {
        let registry = CommandRegistry::new();
        let plugin = Owner::Plugin(PluginId::new("org.example.tool").unwrap());
        registry
            .register(info("tool.a", plugin.clone(), None), echo())
            .unwrap();
        registry
            .register(info("tool.b", plugin.clone(), None), echo())
            .unwrap();
        registry
            .register(info("file.save", Owner::Core, None), echo())
            .unwrap();
        assert_eq!(registry.unregister_owner(&plugin), 2);
        let ids: Vec<_> = registry
            .list()
            .into_iter()
            .map(|c| c.id.to_string())
            .collect();
        assert_eq!(ids, ["file.save"]);
    }

    #[test]
    fn context_prefix_removal() {
        let keys = ContextKeys::default();
        keys.set("tool.ready", json!(true));
        keys.set("tool.mode", json!("x"));
        keys.set("editorFocus", json!(true));
        keys.remove_prefix("tool.");
        assert!(keys.get("tool.ready").is_none());
        assert_eq!(keys.get("editorFocus"), Some(json!(true)));
    }
}

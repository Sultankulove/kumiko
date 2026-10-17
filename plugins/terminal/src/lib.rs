//! kumiko.terminal: terminal sessions (spec, section 19). `terminal.new [title?]` opens a
//! session with the shell of the settings (`terminal.shell`, `terminal.args`); an empty shell
//! leaves the choice to the core (the platform's shell), the working folder is the first
//! workspace folder. Disabling the plugin removes the command.

use std::cell::Cell;

use kumiko_plugin_sdk::wasm::{Plugin, PluginMain};
use serde_json::{Value, json};

thread_local! {
    /// Sessions opened by this instance, for titles: Local, Local (2), …
    static OPENED: Cell<u32> = const { Cell::new(0) };
}

/// The spec of a session from the plugin's settings (relative keys).
pub fn spec(config: &Value) -> Value {
    let shell = config
        .get("shell")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    let args = config.get("args").cloned().unwrap_or_else(|| json!([]));
    json!({ "command": shell, "args": args, "cwd": null })
}

pub fn title(number: u32) -> String {
    if number <= 1 {
        "Local".to_owned()
    } else {
        format!("Local ({number})")
    }
}

struct Terminal;

impl PluginMain for Terminal {
    fn build() -> Plugin {
        Plugin::new().command("terminal.new", |ctx, args| {
            let number = OPENED.with(|n| {
                n.set(n.get() + 1);
                n.get()
            });
            let title = args
                .first()
                .and_then(Value::as_str)
                .map_or_else(|| title(number), str::to_owned);
            let params = json!({ "title": title, "spec": spec(&ctx.params().config) });
            ctx.call("terminal/open", params)
        })
    }
}

kumiko_plugin_sdk::export_wasm_plugin!(Terminal);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_follow_the_settings() {
        assert_eq!(
            spec(&json!({"shell": " bash ", "args": ["-l"]})),
            json!({"command": "bash", "args": ["-l"], "cwd": null})
        );
        assert_eq!(spec(&json!({}))["command"], "");
        assert_eq!(title(1), "Local");
        assert_eq!(title(3), "Local (3)");
    }
}

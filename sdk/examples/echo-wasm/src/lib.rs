//! `kumiko.echo-wasm`: answers commands so the WASM host can be tested against a real component.
//!
//! - `echo-wasm.say` returns its arguments;
//! - `echo-wasm.count` counts its calls in this instance (a restart starts again at 1);
//! - `echo-wasm.trap` traps (a crash, from the host's point of view);
//! - `echo-wasm.spin` loops forever (interrupted by the call budget);
//! - `echo-wasm.grow [mib]` keeps that much memory (fails past the memory limit);
//! - `echo-wasm.host [method, params]` calls a host method (permissions are enforced).

use std::cell::{Cell, RefCell};

use kumiko_plugin_sdk::wasm::{Plugin, PluginMain};
use serde_json::{Value, json};

thread_local! {
    static CALLS: Cell<u64> = const { Cell::new(0) };
    static KEPT: RefCell<Vec<Vec<u8>>> = const { RefCell::new(Vec::new()) };
}

struct Echo;

impl PluginMain for Echo {
    fn build() -> Plugin {
        Plugin::new()
            .command("echo-wasm.say", |_ctx, args| Ok(Value::Array(args)))
            .command("echo-wasm.count", |ctx, _args| {
                let calls = CALLS.with(|c| {
                    c.set(c.get() + 1);
                    c.get()
                });
                Ok(json!({ "id": ctx.plugin_id(), "calls": calls }))
            })
            .command("echo-wasm.trap", |_ctx, _args| {
                // A trap is how a component crashes; `unreachable` is the plainest one.
                std::process::abort()
            })
            .command("echo-wasm.spin", |_ctx, _args| {
                let mut n: u64 = 0;
                loop {
                    n = std::hint::black_box(n.wrapping_add(1));
                }
            })
            .command("echo-wasm.grow", |_ctx, args| {
                let mib = args.first().and_then(Value::as_u64).unwrap_or(1);
                for _ in 0..mib {
                    KEPT.with_borrow_mut(|kept| kept.push(vec![1; 1 << 20]));
                }
                Ok(json!(KEPT.with_borrow(Vec::len)))
            })
            .command("echo-wasm.host", |ctx, args| {
                let method = args.first().and_then(Value::as_str).unwrap_or_default();
                ctx.call(method, args.get(1).cloned().unwrap_or(Value::Null))
            })
    }
}

kumiko_plugin_sdk::export_wasm_plugin!(Echo);

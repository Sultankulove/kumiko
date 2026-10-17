# Как написать плагин для Kumiko

Протокол плагинов (KPP) v1.0.0 заморожен: после этого методы только добавляются, плагин сверяет `hostCapabilities.methods` из `initialize` вместо сравнения версий. Полный каталог методов и событий: [docs/protocol/v1.md](../protocol/v1.md), схема — `sdk/schema/kpp.schema.json`.

## Два вида плагинов

| | Процессный | WASM |
|---|---|---|
| Что это | исполняемый файл, говорит JSON-RPC по stdin/stdout | компонент `wasm32-wasip2` мира `kumiko-plugin` (`sdk/wit`) |
| Когда выбирать | нужны сеть, JDK, чтение вне проекта, долгая работа | логика и декларативный UI; песочница, меньше памяти |
| Права | `permissions` в манифесте проверяет ядро | то же, плюс лимиты памяти и времени вызова |
| Rust SDK | `kumiko-plugin-sdk` с feature `process` | `kumiko-plugin-sdk` с feature `wasm` |
| Эталоны | `sdk/examples/echo`, `plugins/java` | `sdk/examples/echo-wasm`, `plugins/git`, `plugins/terminal`, C: `sdk/examples/hello-c` |

Плагин на другом языке пользуется тем же API: пример на C проходит тот же набор проверок.

## Манифест `plugin.toml`

```toml
[plugin]
id = "my.tool"                 # уникальный; последний сегмент — пространство имён (`tool.*`)
name = "My Tool"
version = "0.1.0"
api = "^1.0"                   # какие версии KPP понимает плагин
kind = "process"               # или "wasm"
entry = "bin/my-tool"          # или "plugin.wasm"
permissions = ["fs.workspace", "process.spawn"]   # fs.home, network, ui — по необходимости
activation = ["onCommand:tool.*", "workspaceContains:**/pom.xml", "onLanguage:java"]

[[contributes.commands]]
id = "tool.run"
title = "Run my tool"

[[contributes.languages]]            # язык, расширения, грамматика (пакет L3)
[[contributes.languageServers]]      # сервер: запускает ядро по `lsp/resolveLaunch`
[[contributes.taskProviders]]        # type = "maven"
[[contributes.views]]                # дерево, которое плагин заполняет через ui/tree/setModel
[contributes.configuration]          # схема настроек плагина, ключи только `tool.*`

[[provides]]                         # интерфейс для других плагинов
[[consumes]]                         # интерфейс, который плагин зовёт (optional = true)
```

Ошибки в манифесте называют файл и поле; плагин с несовместимой версией KPP установится, но не включится, причины видны в окне Problems.

## Что умеет плагин

Всё идёт запросами к ядру (`ctx.call(...)` в SDK):

- **Команды и контекст:** `command/execute`, `command/setContext`.
- **Документы и файлы:** `document/*`, `fs/*` (нужно `fs.workspace`).
- **Процессы:** `process/spawn` (потоки), `process/run` (до конца, вывод строкой), `process/spawnPty`; нужно `process.spawn`. Дерево процессов гасится вместе с плагином.
- **Терминал:** `terminal/open` открывает сессию в IDE, байты идут мимо плагина.
- **Задачи:** `task/provide`, `task/resolve` отвечает плагин, запускает и показывает ядро.
- **Языковые серверы:** `lsp/resolveLaunch` (как запустить), `lsp/middleware/request` (перехват методов, объявленных в `lspMiddleware`), `lsp/sendRequest`, `lsp/applyEdit`.
- **Декларативный UI** (разрешение `ui`): `ui/tree/setModel`, `ui/tree/update`, `ui/fileDecorations`, `ui/setStatusItem`, `ui/showInput`, `ui/showQuickPick`, `ui/showMessage`. Цвета и иконки — токены темы (`vcs.modified`), своего рисования нет.
- **События:** подписка `event/subscribe`; потерянные события приходят как `$/resync`, после него плагин перечитывает состояние.

Плагин получает при `initialize` рабочие папки, свои настройки, локаль и свои каталоги `storage.data` и `storage.cache`.

## Минимальный процессный плагин

```rust
use kumiko_plugin_sdk::process::Plugin;
use serde_json::json;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    Plugin::new()
        .command("hello.say", |_ctx, args| async move { Ok(json!({ "said": args })) })
        .run_stdio()
        .await
        .ok();
}
```

Stdout занят протоколом: логи — только в stderr.

## Минимальный WASM-плагин

```rust
use kumiko_plugin_sdk::wasm::{Plugin, PluginMain};
use serde_json::Value;

struct Hello;

impl PluginMain for Hello {
    fn build() -> Plugin {
        Plugin::new().command("hello.say", |_ctx, args| Ok(Value::Array(args)))
    }
}

kumiko_plugin_sdk::export_wasm_plugin!(Hello);
```

Сборка: `cargo build --release --target wasm32-wasip2`, файл `plugin.wasm` кладётся рядом с `plugin.toml`.

## Установка и проверка

Плагин — папка с `plugin.toml` и точкой входа. Ставится в `<data>/plugins/<id>/<версия>/` (или kit-ом, см. [distributions.md](distributions.md)).

Для проверки без окна используйте headless-драйвер (`crates/kumiko-headless`): примеры в `tests/headless/e4_*`, `e5_*`, `e12_git.rs`. Сбои плагина не должны ронять ядро: `kill -9` процесса, trap WASM и бесконечный цикл покрыты тестами `tests/modularity` и `tests/headless/e5_wasm.rs`.

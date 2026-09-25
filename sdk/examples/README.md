# Примеры плагинов

| Каталог | Плагин | Вид | Язык | Сборка |
|---|---|---|---|---|
| `echo/` | `kumiko.echo` | процесс | Rust, `kumiko-plugin-sdk` (feature `process`) | `cargo build -p kumiko-echo` |
| `echo-wasm/` | `kumiko.echo-wasm` | WASM | Rust, `kumiko-plugin-sdk` (feature `wasm`) | `cargo build -p kumiko-echo-wasm --release --target wasm32-wasip2` |
| `hello-c/` | `kumiko.hello-c` | WASM | C, wasi-sdk | `sdk/examples/hello-c/build.sh <каталог>` |

`echo` и `echo-wasm` — фикстуры тестов Plugin Host: у них есть команды, которые падают, зацикливаются и расходуют память. `hello-c` — эталонный плагин не на Rust (критерий MVP 16). Он показывает, что для WASM-плагина достаточно WIT-пакета `sdk/wit/kumiko-plugin`: протокол KPP идёт через `handle` и `host.call` строками JSON.

Каталог плагина для установки — это `plugin.toml` и файл из `entry` рядом с ним. Headless-тесты собирают такие каталоги сами: `tests/headless/fixtures.rs`.

## C и другие языки

Биндинги для C генерирует `wit-bindgen c` (`hello-c/regen-bindings.sh`); готовые лежат в `hello-c/bindings/`. Для сборки нужен [wasi-sdk](https://github.com/WebAssembly/wasi-sdk): путь берётся из `WASI_SDK_PATH`, по умолчанию `~/.local/wasi-sdk`.

Правила памяти канонического ABI:
- строки, переданные в экспорт, принадлежат плагину, и он их освобождает;
- строки, которые плагин возвращает, должны лежать в куче: их освобождает сгенерированный `post_return`;
- результаты `host.call` освобождает вызывающий.

Другие языки с поддержкой Component Model подключаются так же, через WIT-пакет: JS/TS (jco), Python (componentize-py), Go (TinyGo).

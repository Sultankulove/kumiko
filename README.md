# kumiko

Модульная native IDE на Rust: ядро, UI и плагины держатся вместе только стабильными контрактами, любую деталь можно вынуть и заменить. Первая платформа — Linux, первый язык — Java.

Статус: ранняя разработка, этап E5 (WASM-плагины: wasmtime, SDK, эталонный плагин на C). Полное ТЗ — [docs/spec/kumiko-tz-v1.0.md](docs/spec/kumiko-tz-v1.0.md), решения — [docs/adr/](docs/adr/), протокол плагинов — [docs/protocol/v1.md](docs/protocol/v1.md).

## Сборка

Нужен rustup; версия тулчейна зафиксирована в `rust-toolchain.toml`.

```bash
cargo build --workspace
cargo test --workspace
```

Проверки CI локально:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check
```

## Структура

| Каталог | Что там |
|---|---|
| `crates/` | Ядро (L0) и UI-компоненты (L1), граница модуля = граница крейта |
| `app/kumiko/` | Бинарник: composition root и CLI |
| `sdk/` | WIT-пакет, JSON Schema протокола, Rust SDK для плагинов и [примеры](sdk/examples/README.md) на Rust и C |
| `plugins/` | Встроенные плагины: java (процесс), git и terminal (WASM) |
| `resources/` | Данные L3: темы, локализация, раскладка клавиш, схема настроек ядра |
| `tests/` | Архитектурные, headless- и модульные тесты |
| `docs/` | ТЗ, ADR, исследования, протокол |

Допустимые зависимости между крейтами проверяет `tests/architecture.rs`; он же следит, чтобы в коде ядра и UI не появлялось знание о конкретных языках, сборщиках и VCS.

Схема `sdk/schema/kpp.schema.json` и таблицы в `docs/protocol/v1.md` генерируются из `kumiko-protocol`; тест падает, если они разошлись с кодом. После намеренного изменения протокола:

```bash
KUMIKO_BLESS=1 cargo test -p kumiko-protocol --test artifacts
```

## Лицензия

[Apache-2.0](LICENSE).

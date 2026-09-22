# Architecture Decision Records

Формат — [MADR](https://adr.github.io/madr/): контекст, варианты, решение, последствия. Приоритет при противоречиях: ТЗ < ADR < явное решение владельца (ТЗ, раздел 30).

Новое спорное решение сначала попадает в `proposed/`, после решения владельца переносится в корень с номером.

| ADR | Тема | Статус |
|---|---|---|
| [001](0001-process-model-and-levels.md) | Модель процессов и уровни L0–L3 | Accepted |
| [002](0002-ipc-json-rpc-binary-frames.md) | IPC: JSON-RPC 2.0 в бинарных фреймах | Accepted |
| [003](0003-ui-toolkit.md) | UI-тулкит | Proposed (спайк E6) |
| [004](0004-document-ownership.md) | Владение документами в ядре | Accepted |
| [005](0005-language-api-lsp.md) | Language API = семантика LSP 3.17 | Accepted |
| [006](0006-debugger-api-dap.md) | Debugger API = DAP | Accepted, реализация после MVP |
| [007](0007-config-toml-json-schema.md) | Конфигурация TOML + JSON Schema | Accepted |
| [008](0008-plugin-package-and-manifest.md) | Формат пакета плагина и манифеста | Accepted |
| [009](0009-license.md) | Лицензия проекта: Apache-2.0 | Accepted |
| [010](0010-wasm-plugins-component-model.md) | WASM-плагины: Component Model + WIT, JSON-payload | Accepted |
| [011](0011-tree-sitter-grammars.md) | Грамматики Tree-sitter: .so в MVP | Accepted для MVP |
| [012](0012-position-encoding.md) | Кодировка позиций: UTF-8 внутри | Accepted |
| [013](0013-kits-and-build-profiles.md) | Kits и профили сборки | Accepted |
| [014](0014-plugin-interfaces.md) | Межплагинные интерфейсы | Accepted |
| [015](0015-l3-resources.md) | Ресурсы L3: темы, иконки, l10n | Accepted |
| [016](0016-event-delivery-classes.md) | Классы доставки событий и resync | Accepted |
| [017](0017-shared-runtime-hosts.md) | Общие рантайм-хосты JS/JVM/Python | Proposed (после MVP) |
| [018](0018-l1-runtime-extensibility.md) | Расширяемость L1 в рантайме | Proposed (после MVP) |
| [019](proposed/0019-cross-service-traits.md) | Где живут трейты взаимодействия сервисов | Proposed, нужно решение до E3 |

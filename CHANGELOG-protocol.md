# Kumiko Plugin Protocol — changelog

Каждое изменение `kumiko-protocol` записывается здесь вместе с обновлением `docs/protocol/v1.md`, `sdk/schema/kpp.schema.json` и WIT-пакета (ТЗ, раздел 30, правило 3). До заморозки v1 (этап E14) протокол имеет версию 0.x и может меняться несовместимо; после заморозки несовместимые изменения требуют новой мажорной версии.

## 0.1.0 — 2026-09-23

Первая версия каталога (этап E1).

- JSON-RPC 2.0 конверт, коды ошибок KPP (`RequestCancelled` −32800 … `InterfaceVersionMismatch` −32807).
- Методы пространств `lifecycle`, `command/`, `event/`, `document/`, `fs/`, `config/`, `ui/`, `task/`, `process/`, `lsp/`, `service/`, `$/` по разделу 16 ТЗ.
- Добавлены сверх эскиза ТЗ: `ui/tree/getChildren`, `ui/progress/cancel`, `process/exited`, WIT-функция `on-stream-close`.
- Типизированные payload-ы событий ядра и классы доставки A/B.
- WIT-пакет `kumiko:plugin@0.1.0`, JSON Schema `sdk/schema/kpp.schema.json`.

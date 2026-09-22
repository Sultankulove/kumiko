# ADR-010. WASM-плагины: Component Model + WIT, JSON-payload в v1

- Статус: Accepted
- Дата: 2026-09-23
- Источник: ТЗ v1.0 ([docs/spec/kumiko-tz-v1.0.md](../spec/kumiko-tz-v1.0.md))

## Контекст

Нужен стабильный ABI для плагинов на разных языках; у Rust нет стабильного ABI (P4, P18).

## Рассмотренные варианты

- Нативные `.so` с C ABI.
- Core WASM-модули с собственным ABI.
- WASM Component Model с минимальным WIT-миром и JSON-payload по схеме KPP.

## Решение

wasmtime + Component Model; WIT-пакет `kumiko:plugin@1.0.0` из двух интерфейсов (`host.call/notify/stream-*`, `plugin.handle/on-*`); payload — JSON той же схемы, что у процессных плагинов (ТЗ, разделы 13, 16).

## Последствия

Одна схема методов для обоих видов плагинов, SDK на любом языке пишется поверх двух функций. Типизированные WIT-записи для горячих методов — минорная версия после замеров.

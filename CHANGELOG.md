# Changelog

Версии — по [SemVer](https://semver.org/lang/ru/). Изменения протокола плагинов — в [CHANGELOG-protocol.md](CHANGELOG-protocol.md).

## 0.1.0 — не выпущена

Первый выпуск: MVP 0.1 (ТЗ v1.0, раздел 3), Linux x86_64 и Windows x86_64.

- Workbench на GPUI: дерево проекта, вкладки, split, палитра команд, Quick Open, Problems, Output, темы, en/ru.
- Редактор: мультикурсоры, поиск и замена, сворачивание, подсветка Tree-sitter.
- LSP-клиент: diagnostics, completion, definition, quick fix.
- Java: JDT LS (скачивается при первом запуске), поиск JDK, задачи Maven.
- Терминал, полнотекстовый поиск по проекту.
- Git-плагин (WASM): статус, окно Commit, ветки, pull/push.
- Kits `minimal` и `java`, профили сборки L1.
- Плагины: процессные и WASM, протокол KPP 1.0.0 заморожен.
- Пакеты: `.zip` и `.msi` для Windows, `.tar.gz` и AppImage для Linux; JDK и JDT LS не входят.

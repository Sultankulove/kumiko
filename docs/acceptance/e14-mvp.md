# Приёмка E14: MVP 0.1

Сводка по ТЗ, разделы 3, 23 и 29: что подтверждает каждый критерий и что осталось за людьми. Проверка на Windows (разработка) и в WSL (Linux); ручные сценарии каждого этапа лежат рядом в `docs/acceptance/`.

## 18 критериев готовности MVP (раздел 3)

| № | Критерий | Чем подтверждён | Статус |
|---|---|---|---|
| 1 | Запустить IDE | `kumiko <папка>`, окно открывается сразу ([E7](e7-workbench.md)) | вживую, Windows |
| 2 | Открыть Java Maven project | Проверен на небольшом Maven-проекте ([E10](e10-java.md)); spring-petclinic не прогонялся | **частично** |
| 3 | Дерево проекта | E7, headless `e7_quick_open` | вживую |
| 4 | Открыть Java-файл | E8 | вживую |
| 5 | Редактировать | E8: мультикурсоры, поиск и замена, сворачивание; IME не проверен на Windows/Linux владельцем | вживую, **IME за владельцем** |
| 6 | Подсветка | Грамматика Java как пакет L3 (`e8_syntax`) | вживую |
| 7 | Autocomplete через LSP | `e9_language`, `e10_java` с настоящим JDT LS | вживую |
| 8 | Diagnostics | то же, окно Problems | вживую |
| 9 | Definition | `Ctrl+B`, `e9_language`, `e10_java` | вживую |
| 10 | Поиск по проекту и Quick Open | `e7_quick_open`, `e11_search`, [E11](e11-terminal-search.md) | вживую |
| 11 | Maven test | задача `maven: test`, BUILD SUCCESS ([E10](e10-java.md)) | вживую |
| 12 | Встроенный terminal | PTY, `kumiko-terminal-view`, плагин `kumiko.terminal` ([E11](e11-terminal-search.md)) | вживую, Windows |
| 13 | Git status | `e12_git`, окно Commit ([E12](e12-git.md)) | вживую |
| 14 | Установить и удалить plugin | `e4_process::install_disable_enable_uninstall_at_runtime` | headless |
| 15 | `kill -9` Java-плагина без падения Core | `e4_process::kill_9_…`, `modularity::killing_a_plugin_during_a_request` | headless |
| 16 | WASM-плагин не на Rust | `e5_wasm::criterion_16_plugin_written_in_c` (на CI с wasi-sdk) | CI |
| 17 | Kit `minimal` ↔ `java` | `e13_kits`, `modularity::kits_leave_user_settings_alone` ([E13](e13-kits.md)) | headless |
| 18 | Quick fix из LSP | «Import java.util.List» через JDT LS | вживую |

## Тесты модульности (раздел 29)

`tests/modularity/main.rs`, запуск `cargo test --test modularity`.

| № | Тест | Где |
|---|---|---|
| 1 | Без Java-плагина `.java` открывается как текст | `modularity::without_the_java_plugin` |
| 2 | Без Git-плагина дерево и терминал работают | `modularity::without_the_git_plugin` |
| 3 | `kill -9` плагина во время запроса | `modularity::killing_a_plugin_during_a_request` |
| 4 | `kill -9` языкового сервера | `modularity::killing_the_language_server` |
| 5 | Профиль `alt-editor` собирается | `modularity::the_alt_editor_profile_builds` |
| 6 | Без плагина terminal задачи идут | `modularity::tasks_run_without_the_terminal_plugin` |
| 7 | Trap WASM-плагина | `e5_wasm::wasm_plugin_runs_crashes_and_recovers` |
| 8 | Бесконечный цикл WASM | `e5_wasm::wasm_plugin_runs_crashes_and_recovers`, `memory_limits_and_repeated_traps` |
| 9 | `minimal` → `java` → `minimal` | `modularity::kits_leave_user_settings_alone` |
| 10 | Эталонный плагин не на Rust | `e5_wasm::criterion_16_plugin_written_in_c` |
| 11 | Переполнение очереди → `$/resync` | `kumiko-events`: `class_a_overflow_drops_the_subscription_with_resync` |

## Бюджеты (раздел 23)

См. [benchmarks/results](../../benchmarks/results). Замерено на Windows (release):

| Метрика | Бюджет | Результат |
|---|---|---|
| Открытие файла 5 000 строк до подсветки | < 50 мс | 17 мс (`e8_syntax`) |
| Подсветка после нажатия клавиши, p95 | часть 16 мс | 1,4 мс |
| Quick Open на 50 000 файлах, худший символ | < 30 мс | 18,8 мс (было 75 мс, оценка файлов стала параллельной) |
| Поиск по 50 000 файлам, первые результаты | < 200 мс | 13 мс (весь обход 10,6 с) |
| Холодный старт до первого кадра | < 500 мс | 313 мс |
| RSS через 30 с, окно с небольшим проектом | < 120 МБ | 109 МБ |
| CPU в простое за 60 с | < 1 % | 0,35 % |

Не замерено: RAM с 10 открытыми файлами и средним проектом, активация плагинов, накладные completion, шторм `fs.changed` на `target/`, ключ→пиксель с отрисовкой. Бюджеты блокируют релиз, не мёрж (раздел 23).

## Заморозка KPP v1

Версия протокола 1.0.0 (`PROTOCOL_VERSION`), `api = "^1.0"` в манифестах, пакет WIT `kumiko:plugin@1.0.0`, C-привязки пересозданы. Совместимость: тот же major; изменения после заморозки только добавляют методы, плагины сверяются со списком `hostCapabilities.methods`.

## Что осталось за людьми

- IME на Windows и Linux (условие ADR-003).
- Все ручные сценарии на Linux.
- spring-petclinic: открыть, дождаться JDT LS, пройти критерии 2, 7–9, 11, 18.
- Решение по остаткам из раздела «Отложено» этапов E8–E13.

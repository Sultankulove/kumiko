# Приёмка E9: LSP-клиент

Этап E9 (ТЗ, раздел 28) принимается headless-тестами против фейкового сервера и ручным сценарием в окне. Сценарий проходится на **Windows и Linux** (ADR-020).

## Автоматически

`tests/headless/e9_language.rs`. Сервер — `tests/fixtures/fake-lsp`, его запускает плагин `kumiko.echo` через `lsp/resolveLaunch`. Сервер говорит в UTF-16, поэтому каждая позиция проходит перевод кодировки (ADR-012).

| Тест | Что проверяет |
|---|---|
| `diagnostics_follow_edits_through_the_encoding` | Diagnostics после открытия и после правки; позиция после кириллицы |
| `completion_hover_and_definition` | Completion (+resolve с импортом), hover, definition |
| `code_actions_apply_edits_and_commands` | codeAction (+resolve), команда сервера через `workspace/applyEdit` |
| `a_crashed_server_is_restarted_with_its_documents` | Перезапуск упавшего сервера, повторный `didOpen` |
| `navigation_and_refactoring` | references, documentHighlight, signatureHelp, rename, formatting, documentSymbol |
| `the_plugin_sends_requests_and_intercepts_server_messages` | `lsp/sendRequest` и middleware плагина |
| `server_progress_is_reported` | `$/progress` |

Критерий этапа «Headless: completion, definition, diagnostics, codeAction против фейкового сервера» закрыт первыми тремя тестами.

## Вручную

### Подготовка

Фейковый сервер ставится вместе с плагином `kumiko.echo` в каталог плагинов пользователя:

- Windows: `%LOCALAPPDATA%\kumiko\data\plugins\kumiko.echo\0.1.0\`;
- Linux: `~/.local/share/kumiko/plugins/kumiko.echo/0.1.0/`.

Туда кладутся `sdk/examples/echo/plugin.toml` и `bin/` с собранными `kumiko-echo` и `kumiko-fake-lsp`. Язык `fakelang` — файлы с расширением `.fake`.

После проверки каталог плагина удаляется.

### Сценарий

Файл `notes.fake`:

```
hello TODO world
x.al
hello again
```

| № | Действие | Ожидается |
|---|---|---|
| 1 | Открыть `notes.fake` | `TODO` подчёркнут волнистой линией цвета предупреждения. Внизу «Problems 1». В Problems строка `1:7  TODO found`, щелчок по ней ставит каретку на `TODO`. |
| 2 | Каретка на `TODO` | В строке статуса «TODO found». |
| 3 | Конец строки 2, `Ctrl+Space` | Список у каретки, в нём `alpha` (по уже набранному `al`). `Enter` вставляет `x.alpha`. На вкладке появляется `*`. |
| 4 | Каретка на `TODO`, `Alt+Enter` | «Replace TODO with DONE» (быстрое исправление первым) и «Insert header». `Enter` заменяет на `DONE`, подчёркивание и проблема исчезают. |
| 5 | Каретка на `hello` в строке 3, `Ctrl+B` | Каретка переходит на `hello` в строке 1. |
| 6 | `Ctrl+Q` на `hello` | Всплывает документация `**hello**`. `Esc` закрывает. |

Файл `shop.fake`:

```
class Shop
  def total
    count = 1
    count
```

| № | Действие | Ожидается |
|---|---|---|
| 7 | `Ctrl+F12` | Quick Open в режиме `@`: `Shop` (class), `total` (method) с отступом. `Enter` переходит к символу. |
| 8 | Каретка на `count`, `Alt+F7` | Список использований `shop.fake:3` и `shop.fake:4` с текстом строк. |
| 9 | `Shift+F6`, набрать `amount`, `Enter` | Имя в поле сначала выделено, набор его заменяет. Оба `count` становятся `amount`. В строке статуса «Renamed in 1 file(s)». |
| 10 | `Ctrl+Alt+L` | Пробелы в конце строк удалены. |
| 11 | В конце файла набрать `x = beta(1,` | Над строкой подсказка `beta(int a, String b)`, после запятой жирным `String b`. `Esc` закрывает. |
| 12 | Строка статуса | Справа «fake: Indexing files 50%». |

Сочетания — как в IntelliJ. `Ctrl+P` в редакторе — Parameter Info, вне редактора по-прежнему Quick Open. `Ctrl+Q` в редакторе — Quick Documentation, вне редактора — выход.

Все шаги пройдены на Windows 2026-10-17. Linux — за владельцем.

Известное ограничение: документация (шаг 6) показывается как исходный Markdown, без разметки.

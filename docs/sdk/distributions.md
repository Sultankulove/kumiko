# Дистрибутивы Kumiko: kits и профили сборки

Состав IDE меняется на двух уровнях (ТЗ, раздел 15).

| Что меняется | Как | Нужна пересборка |
|---|---|---|
| Плагины (L2), ресурсы (L3), настройки по умолчанию | **kit** | нет |
| Редактор, UI-тулкит, терминал (L1) | **профиль сборки**, cargo features `app/kumiko` | да |

Дистрибутив — это бинарник с профилем, папка `resources/` (темы, грамматики, встроенные kits), папка `plugins/` с готовыми плагинами и kit по умолчанию.

## Kit

Файл `*.kit.toml`:

```toml
[kit]
id = "my.kit.java"
name = "My Java"
version = "0.1.0"
kumiko = "^0.1"            # для каких версий IDE
exclusive = false          # true: отключить плагины, которых в kit нет

[plugins]                  # идентификатор = допустимые версии
"kumiko.java" = "^0.1"
"kumiko.git" = "^0.1"

[sources]                  # откуда взять не установленный плагин; папка с plugin.toml
"kumiko.java" = { path = "../plugins/java" }

[resources]
theme = "kumiko.theme.darcula"   # становится настройкой workbench.theme

[settings]                 # слой между значениями по умолчанию и настройками пользователя
"editor.fontSize" = 14
```

Недостающий плагин берётся из `[sources]`, иначе из `plugins/<id>` рядом с бинарником (или из `KUMIKO_PLUGINS`). Если источника нет, kit применяется без него, а недостающий плагин сообщается.

Архивы плагинов (`.tar.zst`) в MVP не поддерживаются: источник — папка.

### Команды

```bash
kumiko kit list                     # встроенные kits и kits пользователя; * — применённый
kumiko kit diff java                # что изменится: install, enable, disable, missing
kumiko kit apply java               # применить по id или пути к файлу
kumiko kit export my.kit.id "Name"  # снимок текущего состава в stdout
```

В окне: «Применить набор…» в палитре команд (`Ctrl+Shift+A`). У каждой строки написано, что будет установлено, включено и отключено, выбор строки и есть подтверждение.

- Применение идемпотентно: повторное ничего не меняет.
- Отключённые плагины остаются установленными; повторное применение kit с ними включает их.
- Применённый kit хранится в `<data>/kits/active.kit.toml`, при старте его настройки подхватываются.
- Пользовательские настройки kit не перезаписывает.
- Встроенные: `minimal` (редактор, дерево, поиск, без плагинов; отключает все плагины) и `java` (Java, Git, Terminal).
- Свои kits лежат в `<data>/kits/*.kit.toml` и видны в списке.

## Профили сборки L1

Редактор выбирается cargo features пакета `app/kumiko`:

| Feature | Что даёт |
|---|---|
| `editor-default` (по умолчанию) | `kumiko-editor-view`: редактор E8 |
| `alt-editor` | Свой редактор: дистрибутив кладёт `app/kumiko/src/editor_alt.rs` с `pub struct Editors`, реализующим `kumiko_ui::editor::EditorProvider` |

Без обеих features сборка останавливается с `compile_error!`. Остальные L1-крейты (`kumiko-ui`, `kumiko-terminal-view`) подключаются так же через трейты `EditorProvider`, `TerminalProvider`, поэтому замена редактора не затрагивает ядро и плагины.

```bash
cargo build --release -p kumiko                                          # профиль по умолчанию
cargo build --release -p kumiko --no-default-features --features alt-editor
```

Выбор UI-тулкита (кроме GPUI) в MVP не предусмотрен.

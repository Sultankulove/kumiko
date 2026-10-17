# Приёмка E10: Java-плагин

Этап E10 (ТЗ, раздел 28): JDK detection, загрузка JDT LS, middleware, Maven tasks, quick fixes. Критерии MVP 2, 7–9, 11, 18 и тест «JDT LS недоступен». Проверка на **Windows и Linux** (ADR-020).

## Автоматически

| Тест | Что проверяет |
|---|---|
| `e10_java::without_jdt_ls_java_still_works` | JDT LS недоступен: файл открывается, подсветка есть, сервер остановлен с понятной причиной, ядро отвечает |
| `e10_java::with_jdt_ls_completion_diagnostics_definition_and_quick_fix` | С настоящим JDT LS (`KUMIKO_JDTLS=<папка>`, `--ignored`): `java.serverReady`, diagnostics (8), completion (7), definition (9), quick fix «Import java.util.List» (18) |
| `e10_tasks::tasks_are_listed_run_and_reported` | Задачи плагинов, запуск, вывод, `task.started` / `task.finished`; фазы Maven для `pom.xml` |
| Модульные тесты `kumiko-java` | Поиск JDK, раскладка JDT LS, параметры запуска, Maven-задачи, SHA-256 и распаковка архива; настоящая загрузка — `KUMIKO_NETWORK=1 --ignored` |

Пример запуска теста с JDT LS (папка `server` расширения VS Code Java подходит):

```bash
KUMIKO_JDTLS=/path/to/jdtls cargo test --test headless e10_java -- --include-ignored
```

## Вручную

### Подготовка

- JDK 21 или новее (им запускается JDT LS) и Maven.
- Плагин `kumiko.java`: `plugins/java/plugin.toml`, `plugins/java/schema/` и собранный `kumiko-java` в `bin/`. Установка — в каталог плагинов (до kits в E13).
- JDT LS скачивается сам при первом открытии Java-файла (≈ 50 МБ, версия 1.61.0, проверка SHA-256). Либо указывается готовый: `[java.jdtls] path = "…"` в настройках пользователя.

### Сценарий (spring-petclinic или любой Maven-проект)

| № | Действие | Ожидается |
|---|---|---|
| 1 | `kumiko <проект>` | Дерево проекта (критерий 2). После старта JDT LS в папке проекта не появляются `.project`, `.classpath`, `.settings`. |
| 2 | Открыть Java-файл | Подсветка сразу. Через несколько секунд — ошибки компиляции подчёркнуты, в Problems (критерий 8). |
| 3 | Набрать `System.out.` | Список с `println` (критерий 7). |
| 4 | `Ctrl+B` на вызове метода | Переход к объявлению (критерий 9). |
| 5 | Тип без импорта, `Alt+Enter` | Первым — «Import '…' (java.util)», внутренних классов JDK (`com.sun.*`, `sun.*`) в списке нет. `Enter` добавляет импорт (критерий 18). |
| 6 | Палитра → «Run Task…» (`Alt+Shift+F10`), «maven: test» | Внизу Output, канал «Run: maven: test», в конце `BUILD SUCCESS` и `Process finished with exit code 0` (критерий 11). Maven запускается с JDK проекта в `JAVA_HOME`. |
| 7 | `Ctrl+F2` во время задачи | Задача останавливается вместе с дочерними процессами. |

На Windows шаги 1–6 пройдены 2026-10-17 на небольшом Maven-проекте с JDT LS 1.56 (из расширения VS Code). spring-petclinic и Linux — за владельцем.

`Alt+Shift+F10` на машине с NVIDIA GeForce Experience перехватывается оверлеем (Instant Replay) и до IDE не доходит. В этом случае задачу запускают из палитры команд или переназначают сочетание.

## Отложено

- Переход в классы JDK и библиотек (`jdt://`, `document/resolveContent`) — после E10.
- `java.selectJdk` и элемент строки статуса с JDK — нужен UI API для плагинов (выбор из списка). Пока JDK проекта задаётся настройкой `java.jdk`.
- Gradle — только в ТЗ на будущее (TODO(open-question-2)).

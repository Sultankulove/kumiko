# Приёмка E15: стабилизация и релиз 0.1

Этап E15 (ТЗ v1.1, раздел 9): spring-petclinic, ручные сценарии на Linux, замеры, которых не хватало в E14, проверка имени, сборка релиза.

## spring-petclinic (критерии 2, 7–9, 11, 18)

Автоматически, с настоящим JDT LS 1.61.0 и JDK 21, на Linux:

```bash
git clone --depth 1 https://github.com/spring-projects/spring-petclinic.git
(cd spring-petclinic && ./mvnw test-compile)      # зависимости в ~/.m2
KUMIKO_JDTLS=<папка JDT LS> KUMIKO_PETCLINIC=<checkout> \
  cargo test --test headless e15_petclinic -- --include-ignored
```

| Тест | Что проверяет |
|---|---|
| `e15_petclinic::spring_petclinic_with_jdt_ls` | Проект открывается, classpath Maven разрешён (в completion после `this.owners.` есть `findById` и `count` из Spring Data), в `OwnerController` нет ошибок (2, 7); `Ctrl+B` на `findById` ведёт в `OwnerRepository.java` (9); без `import java.util.Optional` — ошибка и quick fix «Import» (8, 18); `.project`, `.classpath`, `.settings` в проекте не появляются |
| `e15_petclinic::spring_petclinic_maven_test_task` | Задача «maven: test»: 79 тестов petclinic, `BUILD SUCCESS`, код 0 (11) |

Проект копируется во временную папку, checkout не меняется. Оба теста вместе проходят за 43 с.

## Ручной сценарий на Linux

Дистрибутив из `cargo xtask dist --archive` (архив `.tar.gz`), Xvfb 1600×1000 и программный Vulkan (lavapipe), без видеокарты. JDT LS скачан IDE при первом открытии Java-файла (≈ 7 с до `language server ready`).

| № | Действие | Результат |
|---|---|---|
| 1 | `kumiko kit apply java`, затем `kumiko spring-petclinic` | Дерево проекта, меню, нижние панели ([1](e15/1-project.png)) |
| 2 | `Ctrl+Shift+N`, «OwnerControll», `Enter` | Оба файла в списке, контроллер открыт с подсветкой, хлебные крошки ([2](e15/2-highlighting.png)) |
| 3 | `Ctrl+G` 84, новая строка, `this.owners.fi` | Completion из JDT LS с методами Spring Data; ошибка подчёркнута, «Problems 3», текст ошибки в строке статуса ([3](e15/3-completion.png)) |
| 4 | `Alt+F12`, команда в shell | Терминал, вывод команды ([4](e15/4-terminal.png)) |
| 5 | `Alt+0` | Окно Commit «Changes 0», в строке статуса `⎇ main` ([5](e15/5-commit.png)) |

Найдено (идёт в E17):

- Подписи tool windows на боковой полосе («Project», «Commit») выводятся столбиком по букве, а не повёрнутым текстом.
- Попап completion шире редактора: заходит на дерево проекта слева и обрезан краем окна справа.
- После отмены всех правок (`Ctrl+Z`) вкладка остаётся помеченной `*`, хотя текст совпадает с файлом.

IME на Linux (ibus, fcitx) в виртуальном дисплее не проверить — остаётся за владельцем.

## Замеры (раздел 23)

`cargo test --release --test headless e15_bench -- --ignored --nocapture --test-threads 1`; результаты — [benchmarks/results](../../benchmarks/results) (`*-linux.json`). Облачный контейнер, 4 vCPU Xeon 2,1 ГГц, release, без окна.

| Метрика | Бюджет | Результат |
|---|---|---|
| Активация WASM-плагина (из кеша компиляции) | < 50 мс | 4,6 мс (без кеша, с компиляцией — 152 мс) |
| Активация процессного Rust-плагина | < 300 мс | 4,2 мс |
| Накладные расходы WASM-плагина в idle | < 10 МБ на плагин | 1,9 МБ (первый плагин вместе с движком wasmtime — 26,5 МБ) |
| Completion, доля IDE | < 10 мс сверх ответа сервера | p95 0,23 мс (круг с мгновенным фейковым сервером) |
| RAM с проектом из 500 классов, 10 открытых файлов | < 300 МБ | ядро без окна 50 МБ; окно с одним файлом и JDT LS — 155 МБ |
| Шторм `fs.changed`, 10 000 файлов в `target/` | без фризов > 50 мс | худшая задержка ответа ядра 1,6 мс |
| Quick Open, 50 000 файлов | < 30 мс на символ | 13 мс |
| Поиск, 50 000 файлов, первые результаты | < 200 мс | 2 мс |

JDT LS на petclinic — около 1 ГБ RSS (отдельный процесс, без бюджета; раздел 23).

Не замерено на Linux: первый кадр и задержка «клавиша → пиксель» (в Xvfb рендер программный: первый кадр 3,5 с ничего не говорит о машине с видеокартой), CPU в простое с окном. На Windows они замерены в E14 (313 мс, 0,35 %).

## Имя «Kumiko» (open-question-1)

Проверено 20.10.2026:

| Где | Что нашлось |
|---|---|
| crates.io | `kumiko`, `kumiko-core`, `kumiko-app`, `kumiko-cli`, `kumiko-server`, `kumiko-crawler`, `kumiko-macros` заняты с июня 2026 другим проектом (зеркало комиксов, github.com/BppleMan/kumiko). `kumiko-ide` свободно |
| GitHub | Организации `kumiko` в поиске нет; есть пользовательский проект «Kumiko» (инструмент для Revit, Windows) |
| Веб-поиск по IDE и товарным знакам | Совпадений с IDE или редактором не найдено |
| Домены, базы товарных знаков (USPTO, EUIPO, WIPO) | Из контейнера не проверить (нет DNS, нет доступа к базам) — за владельцем |

Вывод для владельца: публиковать крейты под именами `kumiko-*` на crates.io нельзя — нужен префикс (например, `kumiko-ide-*`) или другое имя. Внутри репозитория имена крейтов можно не менять, пока они не публикуются (`publish = false`).

## Сборка релиза

- `cargo xtask dist [--archive]` (`tools/xtask`): release-бинарь, плагин Java, WASM-плагины Git и Terminal, библиотека грамматики Java; раскладка `resources/` и `plugins/<id>/` рядом с исполняемым файлом. На Linux `--archive` даёт `.tar.gz` (24 МБ, распакованный — 82 МБ).
- `.github/workflows/release.yml`: по тегу `v*` — `.tar.gz`, AppImage, `.zip`, `.msi` (WiX 5) и суммы SHA-256 в GitHub Release; PR, меняющий упаковку, собирает пакеты без публикации.
- Проверено на Linux: архив и AppImage запускаются, видят встроенные kits, `kit apply java` ставит три плагина из дистрибутива, повторное применение ничего не меняет.

## Осталось за людьми

- IME на Windows и Linux.
- Проверка доменов и товарных знаков; решение по имени крейтов.
- Установка `.msi` на чистой Windows.

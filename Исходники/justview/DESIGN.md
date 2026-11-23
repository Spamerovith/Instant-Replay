# DESIGN.md — JustView

> Единственный источник значений интерфейса. Значения в коде берутся по имени токена
> из `src/theme.rs`. Новую роль добавляй сюда и в `theme.rs` одним изменением.

Updated: 2026-11-21

## Direction

- **Direction:** house style, premium ultra-minimal (тёмный, монохромный, контент — единственный цвет).
- **Concept:** световой стол в фотолаборатории — в полной темноте светится только снимок,
  инструменты лежат рядом и исчезают, когда на снимок смотрят.
- **Qualities:** тихий, точный, мгновенный.
- **Avoid:** градиенты и свечение; постоянно видимые панели; второй акцентный цвет; рамки вокруг фото.
- **Motion:** спокойно и весомо; панели плавно гаснут через 2 с бездействия; нажатие отвечает
  сразу (сжатие + пружинный возврат ≈1%).
- **Signature details:** (1) «видоискатель» — уголки рамки на пустом экране и в иконке;
  (2) панель управления-капсула, которая растворяется, пока смотришь на кадр.

## Code tokens

| Platform | File | Naming |
| --- | --- | --- |
| native (egui) | `src/theme.rs` | `CANVAS`, `SURFACE`, `TEXT_SECONDARY`, `SP_16`, `T_BODY` |

Units: логические пиксели egui (= DIP Windows). Тема: только тёмная (просмотрщик фото).

## Fonts

| Role | Family | Fallback | Weights | Loading | Notes |
| --- | --- | --- | --- | --- | --- |
| UI / text | Geologica (static instances) | встроенные шрифты egui (символы) | 340, 400, 500 | встроены в exe (`assets/fonts`) | CRSV 0, SHRP 0, slnt 0; полная кириллица |
| Code / text files | Consolas (системный) | Geologica 400 | 400 | загружается из `C:\Windows\Fonts` | только просмотр текстовых файлов |

## Type scale (app scale, desktop tool)

| Role | Token | Size | Weight | Usage |
| --- | --- | --- | --- | --- |
| title | `T_TITLE` | 22 | 500 | пустой экран, заголовок диалога |
| headline | `T_HEADLINE` | 17 | 500 | заголовок панели «Сведения» |
| body | `T_BODY` | 15 | 340 | основной текст |
| subhead | `T_SUBHEAD` | 14 | 400 | кнопки, значения, подписи |
| footnote | `T_FOOTNOTE` | 13 | 400 | время, проценты масштаба, счётчик |
| caption | `T_CAPTION` | 12 | 400 | метки групп, подсказки клавиш |

## Color

| Role | Token | Value | Usage |
| --- | --- | --- | --- |
| canvas | `CANVAS` | #000000 | фон окна и заголовок окна (DWM) |
| surface | `SURFACE` | #161617 | панель «Сведения», диалоги |
| surface-raised | `SURFACE_RAISED` | #1d1d1f | наведение на кнопки, строки, меню |
| glass | `GLASS` | rgb(22 22 23 / 0.86) | только плавающие панели поверх кадра |
| text | `TEXT` | #f5f5f7 | основной текст, иконки |
| text-secondary | `TEXT_SECONDARY` | #a1a1a6 | вторичный текст |
| hairline | `HAIRLINE` | rgb(255 255 255 / 0.1) | разделители списков, дорожка ползунков |
| field-border | `FIELD_BORDER` | #86868b | контур ghost-кнопки |
| action | `ACTION` / `ON_ACTION` | #f5f5f7 / #000 | главная кнопка-пилюля |
| link / focus | `ACCENT` | #2997ff | ссылки, прогресс воспроизведения, фокус, выбранная скорость |
| danger | `DANGER` | #ff453a | удаление, ошибки (всегда с текстом) |

Цвет в интерфейсе — только от контента. Состояние никогда не передаётся одним цветом.

## Spacing

Scale: 4, 8, 12, 16, 20, 24, 32, 40, 48, 64. Tokens `SP_4 … SP_64`.
Внутри контрола 4–8; между кнопками одной группы 4; между группами 12–16; отступ панели от края окна 16–24.

## Radii

| Role | Token | Value |
| --- | --- | --- |
| card | `R_CARD` | 18 (панели, диалоги) |
| field | `R_FIELD` | 12 (меню, строки) |
| pill | `R_PILL` | половина высоты (кнопки, капсула, сегменты) |

## Components

- **Icon button:** 36×36, круг, иконка 18 px, штрих 1.5 px; hover — fill surface-raised; press — сжатие до 1 − 8/(w+h).
- **Capsule toolbar:** высота 52, glass, padding 8, gap 4, по центру снизу, 20 px от края.
- **Media bar:** ширина min(760, окно − 48), glass, R_CARD; ряд 1 — шкала (высота 4, при наведении 6) + время; ряд 2 — кнопки.
- **Speed menu:** пилюля «1×»; меню растёт из кнопки вверх; шаги 0.25, 0.5, 0.75, 1, 1.25, 1.5, 1.75, 2, 2.5, 3.
- **Edge arrows:** круг 44, glass, появляются у левого/правого края при наведении.
- **Info sheet:** справа, ширина 340, surface, строки 44 с hairline-разделителями.
- **Primary pill:** высота 44, padding 22, action fill. Одна на экран.

## Motion

| Role | Token | Value |
| --- | --- | --- |
| press | `M_PRESS` | 80 ms вниз, удержание ≥ 90 ms |
| release | spring | жёсткость 520, затухание 26 (≈1% перелёта) |
| micro | `M_MICRO` | 0.20 s (hover, цвет) |
| small | `M_SMALL` | 0.30 s (меню, подсказки, тосты) |
| medium | `M_MEDIUM` | 0.40 s (панель сведений, диалог) |
| crossfade | `M_FADE` | 0.16 s (смена файла) |
| chrome idle | `CHROME_IDLE` | 2.0 s до скрытия панелей |
| zoom | `M_ZOOM` | экспоненциальное сглаживание ~0.12 s |

Если в Windows отключены «Эффекты анимации», движение заменяется мгновенной сменой, обратная связь остаётся.

## Prohibited in this project

slop:ai-palette, slop:gradient-text, slop:lazy-cool, slop:background-glow, slop:icon-tile,
slop:identical-card-grid, рамки и тени вокруг фото.

## Discrepancies and exceptions

| Rule | Scope | Reason | Source |
| --- | --- | --- | --- |
| Glass без размытия | плавающие панели | egui не умеет backdrop-blur; заменено плотностью 0.86 | ограничение платформы |
| Тело 15/340 вместо 17 | весь UI | плотный рабочий инструмент | house-style §4 (dense tools) |

### Audio-track pill (1.1.0)

Shown left of the speed pill only when a file has 2+ audio tracks. Same pill as speed
(`SURFACE_RAISED`, height 30, radius 15, `T_FOOTNOTE`), leading three-bar glyph in
`TEXT_SECONDARY`, width 72–168 with ellipsis. Its menu grows upward from the pill like the
speed menu: rows 34, `T_SUBHEAD`, check + `ACCENT` for the selected track, width 200–360.

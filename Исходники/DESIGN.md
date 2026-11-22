# DESIGN.md — Instant Replay

Single source of interface values. Code mirror: `src/ui/theme.rs` (keep in sync).

## Direction

- **Direction:** house style (premium ultra-minimal), adapted to a native Windows desktop tool
  (egui). The user asked for a dark theme; everything else is the house style's app scale with
  dense-tool density.
- **Concept:** a camera's tally light in a dark studio. The interface is black and quiet;
  the only strong colour is the red recording light, shown only while something is actually
  being recorded. The replay buffer is a strip of tape that fills up.
- **Qualities:** calm, exact, instant. **Anti-qualities:** gamer neon, RGB gradients, busy HUD.
- **Signature detail:** the tape bar on the Home hero — a 4 px track that fills to the buffered
  time, with the buffered duration as the leading number above it.
- **Pool effects:** none.

### Deviations from the house style (with reasons)

| Item | House style | Here | Reason |
| --- | --- | --- | --- |
| Font | Geologica (bundled) | Segoe UI (Windows system font) | Native Windows app; no download/bundle needed, full Cyrillic, crisp ClearType-tuned hinting at 12–15 px. Geologica can be bundled later on request. |
| Headline weight | 500 | 600 (Segoe UI Semibold) | Segoe UI ships no 500 static face; egui cannot render variable weights. |
| Icons | Lucide / SF Symbols | Segoe Fluent Icons (system font) | The platform's own thin icon set, coherent with Windows. |
| Warning colour | — | `warning #ff9f0a` | Overlay toast needs a non-error caution mark (always with text). |

## Color roles (dark only — the app is dark by request)

| Token | Value | Use |
| --- | --- | --- |
| `canvas` | `#000000` | Window background, sidebar |
| `surface` | `#161617` | Groups, cards |
| `surface-raised` | `#1d1d1f` | Inputs, dropdowns, keycaps, menus, overlay toast |
| `surface-hover` | `#232325` | Hover fill on rows/controls on surface |
| `surface-pressed` | `#2a2a2c` | Pressed fill step |
| `text` | `#f5f5f7` | Primary text |
| `text-secondary` | `#a1a1a6` | Values, captions, meta (≥ 6.5:1 on surface) |
| `text-disabled` | `#6e6e73` | Disabled labels only (never needed to complete a task) |
| `hairline` | `rgba(255,255,255,0.10)` | Row separators |
| `field-border` | `#86868b` | Ghost pill outline, slider track fill-less part border |
| `focus` | `#2997ff` | Focus ring, keycap listening state, links, slider fill |
| `record` / `danger` | `#ff453a` | Recording light, destructive state — always with text |
| `success` | `#30d158` | Saved mark — always with text |
| `warning` | `#ff9f0a` | Caution mark — always with text |
| `track` | `#3a3a3c` | Slider / switch-off / tape track |
| `track-hover` | `#48484a` | Switch-off track on hover |
| `segment-selected` | `#6e6e73` | Segmented indicator (3.4:1 vs surface-raised; label also turns semibold) |
| `action` | fill `#f5f5f7`, label `#000000` | The one primary pill per screen |

## Typography (egui points = DIP)

| Role | Size / line | Weight |
| --- | --- | --- |
| `display` | 40 / 44 | 600 — Home hero number |
| `title` | 22 / 28 | 600 — page titles |
| `headline` | 15 / 20 | 600 — group titles, card titles |
| `body` | 14 / 20 | 400 — row labels, buttons |
| `body-strong` | 14 / 20 | 600 — selected nav item |
| `subhead-strong` | 13 / 18 | 600 — selected segment |
| `subhead` | 13 / 18 | 400 — values, secondary lines |
| `caption` | 12 / 16 | 400 — meta, version |

Numerals are tabular where they change live (timers, sizes).

## Spacing scale

`4, 8, 12, 16, 20, 24, 32, 40`. Page padding 32. Group inner padding 16 horizontal.
Gap between groups 24. Group title → group 8. Proximity: inside control < between rows < between groups ≤ page padding.

## Shape and size

| Token | Value |
| --- | --- |
| `radius-card` (hero cards) | 20 |
| `radius-group` | 16 |
| `radius-input` | 10 |
| `radius-pill` | full |
| `button-lg` / `md` / `sm` height | 40 / 32 / 26 |
| `row-min-height` | 48 |
| `switch` | 40 × 24, thumb 18 |
| `segmented` height | 30, inner radius = outer − 3 |
| `slider` track / thumb | 4 / 18 |
| `nav-item` height | 36, sidebar width 220 |
| `overlay` panel | width 400, full work-area height, `surface` fill, rows 48 / 56 with hotkey line, gallery tiles 72 high (3 per row) |
| `keycap` height / min width | 30 / 168 (one width for the whole column) |

## Motion

| Token | Value | Use |
| --- | --- | --- |
| `press-down` | 80 ms ease-out, scale 0.96 + fill step | every button, keycap, nav item |
| `press-release` | 240 ms ease-out | return from press (min held 90 ms) |
| `hover` | 160 ms | fill / colour |
| `switch` | 200 ms ease-out | thumb travel + track colour |
| `indicator` | 260 ms ease-out | nav selection, segmented indicator slide |
| `value` | 300 ms ease-out | tape bar fill |
| `page` | 160 ms | page crossfade |
| `toast` | in 180 ms (slide 10 px) / hold 2400 ms / out 280 ms | in-game overlay |

Character: calm and weighty; nothing bounces. Reduced motion: when Windows "Show animations
in Windows" is off, every duration becomes 0 (state changes still happen instantly; press
fill step and focus ring remain).

## Clip player (1.2.0)

Full-window view that replaces sidebar + page while a clip is open. Top row 56 (back icon
button, `headline` title, link «Показать в папке»), black stage, bottom bar 96 separated by a
1 px `SURFACE_RAISED` line. Seek track `SLIDER_TRACK` 4 → 6 on hover, played part `FOCUS`,
thumb appears on hover. Buttons are `icon_button`; volume is `slider` 104 wide. Speed and
audio-track pills: height `BUTTON_MD`, `SURFACE_RAISED` → `SURFACE_HOVER` on hover, press
scale `PRESS_SCALE`, `subhead`; non-default speed in `FOCUS`. Menus grow upward from their
pill (`INDICATOR` fade + 8 px lift), rows 34, radius `GROUP`, selected row with check in `FOCUS`.

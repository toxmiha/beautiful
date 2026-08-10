# Расследование: Blend Modes при transform/deform

Код **не менялся**. Цель этого файла — закрыть доказательства без реализации.

Эпистемика:
- **Confirmed** — эксперименты пользователя или прямое чтение кода (gate / вызов / хардкод).
- **Hypothesis** — наиболее вероятное по call graph; **не** доказано Bench/F12.
- **Open** — альтернатива ещё не исключена.

---

## Что уже подтверждено экспериментами (не гипотезы)

| # | Факт |
|---|---|
| 1 | Сверху только Normal → transform гладкий |
| 2 | Сверху Normal + любая Opacity → гладкий |
| 3 | Сверху **любой** non-Normal (Multiply, Overlay, Screen, Soft/Hard Light, …) → сильный лаг |
| 4 | Blend выше, ROI **не** пересекает его пиксели → лага нет |
| 5 | ROI касается хотя бы нескольких пикселей blend → лаг (в т.ч. пустой 4K) |
| 6 | Blend **ниже** трансформируемого → лага нет |
| 7 | Transform самого blend-слоя → preview как Normal (own blend игнорируется) — баг |
| 8 | Во время transform Opacity float = 100% — баг |

---

## Что код подтверждает про **гейт** (не про hotspot)

| Наблюдение | Код | Вердикт |
|---|---|---|
| Лаг только если non-Normal **выше** слота | `above_cache_ok` смотрит `layers[idx+1..]` | Confirmed |
| Blend ниже слота не включает slow path | тот же skip | Confirmed |
| Без пересечения ROI с bounds → дешевле | `layer_contributes` → `content_bounds().intersects(rect)` | Confirmed |
| Own blend / opacity в overlay | egui mesh `Color32::WHITE`; нет `blend_over(layer.mode)`; `blend_floating_only` хардкодит Normal | Confirmed |

**Gate (Confirmed):**

```
transform_overlay_ok() == above_cache_ok(layers, floating_idx)
transform_above_needs_backdrop() == !overlay_ok && floating_overlay_only && floating.is_some()
```

Любой `effective_blend_mode != Normal` (или clip-to-below) среди видимых непрозрачных слоёв **выше** float → transparent Normal above-plate считается неверным → включается hybrid path `ensure_xform_above_*`.

Это объясняет **почему включается другой путь**. Это **не** измерение, где внутри пути тратятся миллисекунды.

---

## Call graph slow path (Confirmed как структура; hotspot — Hypothesis)

```
MouseMove / warp drag
  → mark_xform_above_live_stale()
  → ensure_xform_above_live_tex_ex()   // только если needs_backdrop
       1. extract_lod(roi, lod)          // ROI = float OBB + pad 24
       2. softlight_blit_posed_float()   // Free posed → apply_free_transform_rgba(FULL baseline)
                                         // Warp edited → mesh_warp_rgba_ex
                                         // else blit floating pixels
       3. pix.clone()                    // полный буфер ROI/lod
       4. bake_transform_above_on_backdrop_lod()
            → blend_above_into[_lod]
                 → для каждого above: layer_contributes?
                      no  → skip layer
                      yes → blend_layer / per-pixel blend_pixel_mode  // по ВСЕМУ rect, не по ∩ bounds
       5. punch_unchanged_to_transparent(after, before)
       6. egui texture set / load_texture
```

Fast path (Normal above only): frozen underlay + GPU float pose + cached transparent `transform_above_plate()` upload. **Не** вызывает шаги 1–6 выше на каждый drag.

---

## Существующие кэши (инвентарь)

### 1. `VisibilityBackdrop` (`visibility_cache.rs`)

| Поле / API | Что хранит | Когда строится | При non-Normal выше |
|---|---|---|---|
| `below` | фон + layers[0..idx] | `ensure` / `ensure_transform_plates` | **используется** (underlay) |
| `above` | layers[idx+1..] на transparent | только если `above_usable` | **не строится** (`above_usable=false`) |
| `on` / `off` | eye spam snapshots | visibility UI | не transform live |
| `plate_gen` | `content_revision` | key | ниже+выше pixels invalid при content bump |
| `above_usable` / `above_cache_ok` | gate | каждый ensure | **false** → no transparent above plate |

`ensure_transform_plates`: комментарий — above на transparent для overlay; для non-Normal above plate **некорректен** (mix-blend нужен backdrop под src).

### 2. App-side transform textures (`canvas/mod.rs`)

| Кэш | Используется когда | Отключается / не помогает при non-Normal |
|---|---|---|
| `xform_above_tex` | Normal: plate upload; non-Normal: static backdrop bake view | live drag всё равно пересобирает live tex |
| `xform_above_live_tex` | non-Normal live | throttle 50ms drag / 120ms idle; **каждый stale** → полный путь 1–6 |
| `xform_underlay_frozen` | skip underlay sync после первого present | **не** кэширует blend-above поверх меняющегося float |
| `transform_baseline` | исходные пиксели float | при Free posed **пересэмплируется заново** в softlight_blit |

### 3. Другие кэши в редакторе (не решают этот путь)

| Кэш | Назначение | Transform blend preview |
|---|---|---|
| `StrokeStack` | brush below-active | не используется в xform above bake |
| `Composite` dense + dirty | document display | underlay freeze / extract_lod читает composite |
| Layer thumbs / nav | UI | нет |

### Выводы по кэшам (без выбора фикса)

- **Уже есть** below plates + frozen underlay + Normal above plate + throttled live tex.
- **Отключается при non-Normal выше:** transparent `above` plate (`above_usable=false`).
- **Между MouseMove можно переиспользовать (гипотеза переиспользования, не доказательство достаточности):**
  - underlay / below (уже),
  - static backdrop без float (частично: `xform_above_tex` static path),
  - Soft Light layer tiles (immutable на drag),
  - baseline pixels (есть; но live posed resample всё равно гоняется),
  - результат blend-above **нельзя** просто memcpy, пока backdrop под blend меняется от float pose.
- **Полный пересчёт ROI:** код сейчас при `layer_contributes==true` гоняет blend по **всему** OBB rect, не по `bounds ∩ ROI`. Это объясняет «несколько пикселей касания → лаг» на уровне алгоритма, но не доказывает, что именно `blend_pixel_mode` — главные ms.

---

## Где теряются Opacity и own BlendMode (Confirmed)

1. Free/Warp live paint: вершины / tint **`Color32::WHITE`** — нет `layer.opacity`.
2. Overlay float **нигде** не вызывает `blend_over(..., layers[idx].blend_mode)`.
3. `VisibilityBackdrop::blend_floating_only` хардкодит `BlendMode::Normal`.

Это отдельные баги корректности того же preview pipeline; не требуют утверждения про ms в `blend_layer`.

---

## Open source: transform preview + blend modes

| Источник | Статус | Суть |
|---|---|---|
| Open paint peer — in-stack transform preview | Confirmed | Два режима: **in-stack** (корректные blend/masks, дороже) vs **overlay** над стеком (быстрее, blend стека неверный). Pref: можно выключить in-stack → overlay. |
| Open paint peer — transform mask + Instant Preview | Confirmed | Асинхронная/LOD-подобная регенерация projection; forced instant preview для тяжёлых tool. |
| Open paint peer (optional composited preview) | Confirmed (ранее) | composited preview часто optional/off |
| **Skia / WebRender** | Confirmed | `mix-blend-mode` требует изоляции + корректный backdrop |
| **closed peers (hypothesis only)** | Hypothesis | закрытые GPU stack; не использовать как «доказанный алгоритм» |

**Сравнение с Beautiful (без выбора решения):**

- Beautiful уже близок к dual-path: Normal above ≈ overlay; non-Normal ≈ попытка «локального in-stack» через CPU bake `blend_above_into` + punch.
- Open peers платят за корректность in-stack scheduler/projection; Beautiful платит синхронным CPU ROI на UI thread throttled ~20 Hz.
- Идея «локально применить» из OSS: не новый compositor, а (а) измерить, (б) сузить работу до ∩ bounds / переиспользовать immutable above tiles / не дублировать resample. Любая из этих идей — **кандидат после Bench**, не план реализации сейчас.

---

## Самокритика и уверенность

### Что может опровергнуть гипотезу «hotspot = blend_layer»

1. Bench покажет, что `apply_free_transform_rgba` / `mesh_warp` внутри `softlight_blit` съедает большую часть wall (особенно Free scale/rotate).
2. `extract_lod` + `pix.clone()` + `punch_unchanged` + texture upload доминируют; blend мал.
3. Лаг есть при **translation-only** (без posed resample) **и** при tiny Soft Light ∩ ROI — тогда resample менее вероятен; если при translation-only лаг исчезает, а при rotate появляется — hotspot скорее resample.
4. F12 покажет `egui` / present / GPU sync, а не CPU composite.

### Альтернативы ещё не исключены

| Кандидат | Почему возможен | Как отличить |
|---|---|---|
| `blend_above_into` / `blend_pixel_mode` | O(ROI)×non-Normal; только на slow path | A/B: Normal vs Multiply, одинаковый Free translate |
| `apply_free_transform_rgba` в softlight_blit | полный baseline каждый live bake при posed | A/B: translate-only vs rotate при том же Multiply |
| `extract_lod` | копия ROI из dense composite | сравнить ROI size |
| `pix.clone` + punch | O(ROI) лишние проходы | инструментировать |
| Texture upload | `load_texture` / `set` каждый stale | GPU/egui spans |
| Лишняя синхронизация underlay | если freeze ломается | `skip_sync` counters |
| Аллокации | scratch vec в `blend_above_into` каждый вызов | alloc/peak Bench |

### Чего не хватает в измерениях

1. **Нет** `perf_scope` внутри `ensure_xform_above_live_tex_ex` (extract / blit / bake / punch / upload) — текущий F12 не разрежет hotspot.
2. MCP **не умеет** драйвить Free/Warp drag → автоматический Bench transform без участия пользователя невозможен.
3. Старый dump `composite_region ~432ms` — **не** атрибуция к `blend_layer`; может быть другой путь.

### Уверенность (%)

| Утверждение | % |
|---|---|
| Gate = any non-Normal (или clip) above float | **95** |
| ROI ∩ content_bounds включает дорогой layer в bake | **90** |
| Slow path = шаги 1–6 выше | **90** |
| Own blend/opacity теряются в overlay paint / Normal hardcode | **95** |
| **Главный hotspot ms = `blend_layer`** | **40** (вероятная гипотеза, не факт) |
| Главный hotspot = Free/Warp resample в softlight_blit | **25** |
| Главный hotspot = extract+clone+punch+upload | **20** |
| «Нужен sandwich rewrite» как единственный фикс | **15** — преждевременно |
| Локальный фикс в transform preview pipeline возможен | **70** (гипотеза предпочтения, не доказательство) |

---

## Bottleneck — формулировка (исправленная)

**Не утверждать:** «функция, которая включает лаг: `VisibilityBackdrop::blend_layer`.»

**Утверждать:**

1. **Confirmed:** любой non-Normal выше включает `transform_above_needs_backdrop` → `ensure_xform_above_live_tex_ex` на drag.
2. **Hypothesis (наиболее вероятная по коду):** внутри этого пути существенная доля CPU — `bake_transform_above_on_backdrop_lod` → `blend_above_into` → `blend_layer` / `blend_pixel_mode` по ROI.
3. **Open:** та же цена может сидеть в resample float, extract_lod, clone/punch, texture upload.
4. **Доказать** только Bench/F12 с разбивкой: backdrop extract; blend; resample; texture upload; GPU; egui; present. Если blend мал — сменить вывод.

---

## Что нельзя делать сейчас

- Не писать код фикса.
- Не предлагать новый renderer / compositor / GPU rewrite / Document / Projection / StrokeStack / Brush.
- Не подгонять архитектуру под sandwich как уже выбранное решение.
- Не называть sandwich «решением», пока нет распределения времени.

## Что можно после доказательств (направление, не план)

Предпочтительный исход расследования: **локально** сузить/переиспользовать работу в transform preview pipeline (существующие plates/caches), починить own blend + opacity, не трогая Normal overlay.

Большой рефакторинг — только если измерения покажут, что локально невозможно.

---

## Bench protocol (когда пользователь готов тащить transform)

MCP app online; transform drag MCP не эмулирует — нужен ручной drag.

**A.** Transform слой под Multiply (ROI пересекает) — 3–5 с drag.  
**B.** Тот же слой, ROI **не** пересекает Multiply.  
**C.** Transform над Multiply (blend ниже) — контроль.  
**D.** Multiply above + Free **translate-only** vs **rotate** — отделить blend от resample.

До/после: `bench_begin` → drag → `bench_end` + `perf_snapshot`.  
Идеально: временно добавить scopes в `ensure_xform_above_live_tex_ex` (только для измерения).

---

*Обновлено: принятие критики по уверенности и premature sandwich; кэши + OSS без выбора фикса.*

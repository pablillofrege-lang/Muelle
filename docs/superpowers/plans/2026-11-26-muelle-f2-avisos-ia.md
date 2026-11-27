# F2 · Avisos de IA — plan de implementación

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** que la isla avise en ámbar al 80 % y en rojo al 95 % del peor límite de Claude y Codex, con la hora de vuelta, la isla elástica de 8 s, el latido finito y el detalle al pasar el ratón, todo tras `menuBarAiAlerts`.

**Architecture:** la lógica pura (peor límite, nivel, frase de hora, máquina de avisos) va en `usage.rs` y un módulo nuevo `alerts.rs`, con pruebas unitarias. `menubar.rs` solo consulta esa lógica al dibujar y lanza animaciones de composición finitas; el único redibujado tras el aviso lo dispara el fin de un `CompositionScopedBatch`, no un temporizador.

**Tech Stack:** Rust 2024, windows-rs 0.62 (DirectComposition `Windows.UI.Composition`), chrono; Ajustes en Tauri 2 + React (Vitest).

**Spec:** `docs/superpowers/specs/2026-11-26-muelle-f2-avisos-ia-design.md`

## Global Constraints

- Todo condicionado a `Config::menu_bar_ai_alerts` (JSON `menuBarAiAlerts`), `false` por defecto. Con `false`, el dibujo y el comportamiento son idénticos a hoy (incluido `main_window()` en la isla y el panel actual al clic).
- Colores: ámbar `#f6b73c` (80-94 %), rojo `#ff5a4a` (≥ 95 %); por debajo del 80 %, el color de marca actual.
- Manda el peor límite: el de `used_percent` más alto; con empate, el primero (5 h).
- Aviso largo 8 s; latido: 3 ciclos de escala 1 → 1,24 → 1 → 1,14 → 1 en 1,4 s cada uno (fotogramas clave 0, 0,12, 0,24, 0,34, 0,44 y 1,0), luego quieto.
- Detalle al pasar el ratón tras 300 ms quieto (un `SetTimer` de un disparo solo mientras el puntero está sobre el uso; `KillTimer` al salir o al abrir).
- Ensanche y frase corta solo en `IslandStyle::Notch`. En `Bubble`: colores, latido y detalle al pasar el ratón.
- Ningún temporizador ni animación activa en reposo. Dock en reposo < ~40 MB.
- Credenciales: solo lectura (no se toca la lectura de cuota).
- Commits en español, concisos, sin `Co-Authored-By`; push tras cada commit.
- CI: `cargo fmt --all --check; cargo clippy --workspace --all-targets -- -D warnings; cargo test --workspace --locked`. Ajustes: `cd apps/settings; pnpm test; pnpm build`.

## Reparto

- Tareas 1-2: Sonnet (Agent nativo), en serie, sin tocar `menubar.rs` salvo el campo de `MenuBarWidgets`.
- Tareas 3-5: Codex `gpt-6.2-sol` high, un solo encargo en serie (comparten `menubar.rs`).
- Tarea 6: Opus (rebase, revisión, instalación), Fable (comparación visual) y Haiku (RAM y CPU).

## Review Focus

1. `resets_at` ausente o ya pasado (dato viejo): la frase larga queda «Claude casi agotado» sin hora y no hay frase corta; nada de «vuelve a las 00:00». → prueba en Tarea 2.
2. Proveedor no `Ready` (cargando, sin sesión, no disponible): nivel normal, sin aviso ni colores. → prueba en Tareas 2 y 3.
3. Porcentaje ≥ 100 (Codex lo da): crítico, anillo lleno, sin desbordar. → prueba en Tarea 2.
4. Cambio de día: ahora 23:50, reinicio 00:30 → «mañana a las 00:30». Reinicio a 6 días → «el <día>.». → prueba en Tarea 2.
5. Apagar `menuBarAiAlerts` en mitad de un aviso: la isla vuelve a su ancho, se paran las animaciones y no queda ningún lote pendiente que redibuje en ancho largo. → prueba nativa en Tarea 4.

---

### Task 1: Clave `menuBarAiAlerts` y su interruptor en Ajustes

**Files:**
- Modify: `crates/muelle-config/src/lib.rs` (campo junto a `menu_bar_bare`, ~125; default ~402; prueba ~1000)
- Modify: `apps/dock/src/menubar.rs:216-230` (`MenuBarWidgets`)
- Modify: `apps/dock/src/app/preferences.rs:316-323`
- Modify: `apps/settings/src/config.ts:54`, `apps/settings/src/App.tsx` (~693, misma `SubRow` que `menuBarBare`), `apps/settings/src/i18n*`

**Interfaces:**
- Produces: `Config::menu_bar_ai_alerts: bool`; `MenuBarWidgets::ai_alerts: bool`; TS `menuBarAiAlerts?: boolean`; textos `t.menuBarAiAlerts` («Avisos de límite de IA») y `t.menuBarAiAlertsHint` («Ámbar al 80 %, rojo y hora de vuelta al 95 %»).

- [ ] **Step 1:** ampliar la prueba existente de claves nuevas en `lib.rs` (~1000): `assert!(!fresh.menu_bar_ai_alerts)`, `assert!(!older.menu_bar_ai_alerts)` y que `{"menuBarAiAlerts":true}` da `true`.
- [ ] **Step 2:** `cargo test -p muelle-config` → FAIL (campo no existe).
- [ ] **Step 3:** añadir el campo con `#[serde(default)]` como `menu_bar_bare`; pasarlo a `MenuBarWidgets.ai_alerts` en `apply_menu_bar_setting`. Interruptor en Ajustes (es/en; el resto de idiomas, el texto inglés).
- [ ] **Step 4:** `cargo test -p muelle-config` y `cd apps/settings; pnpm test` → PASS.
- [ ] **Step 5:** commit «Añade la clave de avisos de IA» y push.

### Task 2: Peor límite, nivel y frase de hora

**Files:**
- Modify: `apps/dock/src/usage.rs` (impl de `ProviderUsage` ~89-106; pruebas al final)
- Modify: `apps/dock/src/strings.rs` y `apps/dock/src/strings/*.rs` (8 idiomas)

**Interfaces:**
- Produces:
  - `pub enum Level { Normal, Warn, Critical }` (`Clone, Copy, PartialEq, Eq, Debug`)
  - `pub fn level(percent: u8) -> Level` — `<80` Normal, `80..=94` Warn, `>=95` Critical.
  - `impl ProviderUsage { pub fn worst_window(&self) -> Option<&Window>; pub fn alert_level(&self) -> Level }` — `alert_level` es Normal si `state != Ready` o no hay ventana.
  - `pub fn back_phrase(resets_at: Option<DateTime<Utc>>, now: DateTime<Local>, long: bool) -> Option<String>` — `None` si no hay hora o ya pasó.
  - `pub fn alert_text(provider: Provider, usage: &ProviderUsage, now: DateTime<Local>) -> String` — frase larga completa.
  - Campos nuevos en `strings::Text`: `usage_almost_out` («{name} casi agotado»), `usage_back_today_long` («vuelve a las {time}»), `usage_back_tomorrow_long` («vuelve mañana a las {time}»), `usage_back_day_long` («vuelve el {day} a las {time}»), `usage_back_today` («vuelve {time}»), `usage_back_tomorrow` («vuelve mañana {time}»), `usage_back_day` («vuelve {day} {time}»), `weekdays_short: [&str; 7]` (lunes primero: «lun.», «mar.», «mié.», «jue.», «vie.», «sáb.», «dom.»). Inglés: «{name} almost out», «back at {time}», «back tomorrow at {time}», «back {day} at {time}», «back {time}», «back tomorrow {time}», «back {day} {time}», `["Mon","Tue","Wed","Thu","Fri","Sat","Sun"]`. Los otros 6 idiomas copian el inglés.

- [ ] **Step 1: pruebas que fallan** (con `strings` en español fijado como hagan las pruebas existentes; si no hay forma, probar con un `Text` pasado como parámetro a una variante `back_phrase_with(text, …)`):
  - `level(79)==Normal`, `level(80)==Warn`, `level(94)==Warn`, `level(95)==Critical`, `level(130)==Critical`.
  - `worst_window`: 5 h 30 % + 7 d 96 % → el de 7 d; 5 h 50 % + 7 d 50 % → el de 5 h; sin ventanas → `None`.
  - `alert_level`: `State::Unavailable` con 99 % → Normal.
  - `back_phrase` con `now` = miércoles 2026-11-25 21:00 local: reinicio 21:40 hoy → largo «vuelve a las 21:40», corto «vuelve 21:40»; jueves 04:00 → «vuelve mañana a las 04:00» / «vuelve mañana 04:00»; sábado 04:00 → «vuelve el sáb. a las 04:00» / «vuelve sáb. 04:00»; `now` 23:50 y reinicio 00:30 del día siguiente → «vuelve mañana a las 00:30»; reinicio en el pasado → `None`; `None` → `None`.
  - `alert_text(Claude, 97 % con reinicio 21:40)` → «Claude casi agotado, vuelve a las 21:40»; sin hora → «Claude casi agotado».
- [ ] **Step 2:** `cargo test -p muelle usage::` → FAIL.
- [ ] **Step 3:** implementar. Las pruebas comparan por fecha local (`date_naive`) para hoy/mañana; «el día» para cualquier otro.
- [ ] **Step 4:** `cargo test -p muelle usage::` → PASS; `cargo clippy` limpio.
- [ ] **Step 5:** commit «Calcula el peor límite y la hora de vuelta» y push.

### Task 3: Colores de estado en el anillo, el porcentaje y el panel

**Files:**
- Modify: `apps/dock/src/gfx.rs:369` (`usage_ring`)
- Modify: `apps/dock/src/menubar.rs` `draw_compact_usage` (~4928), `draw_bubble_usage` (~5097), `draw_usage_row` (~1281)

**Interfaces:**
- Consumes: `Level`, `level`, `worst_window`, `alert_level` (Tarea 2); `MenuBarWidgets::ai_alerts` (Tarea 1).
- Produces: `pub(crate) const ALERT_WARN: Color` (`#f6b73c`), `ALERT_CRITICAL: Color` (`#ff5a4a`); `fn alert_ink(level: Level, accent: Color) -> Color` en `menubar.rs`; `usage_ring` limita `percent` a 100 al calcular los segmentos.

- [ ] **Step 1:** pruebas: `alert_ink(Normal, accent)==accent`, `alert_ink(Warn, _)==ALERT_WARN`, `alert_ink(Critical, _)==ALERT_CRITICAL`; y una prueba pura de que con `ai_alerts=false` la ventana mostrada es `main_window()` y con `true` es `worst_window()` (extraer `fn shown_window(usage, ai_alerts) -> Option<&Window>`).
- [ ] **Step 2:** `cargo test -p muelle menubar::` → FAIL.
- [ ] **Step 3:** con `ai_alerts`, la isla (Muesca y Burbuja) usa `shown_window` para «NN%» y el anillo, y pinta ambos con `alert_ink`. El panel actual (sin el rediseño de la Tarea 5) colorea su porcentaje y su barra igual. Sin `ai_alerts`, nada cambia.
- [ ] **Step 4:** `cargo test --workspace --locked` → PASS.
- [ ] **Step 5:** commit «Colorea el uso de IA según el límite» y push.

### Task 4: Aviso de la isla elástica y latido

**Files:**
- Create: `apps/dock/src/alerts.rs` (declarar `mod alerts;` en `main.rs`)
- Modify: `apps/dock/src/menubar.rs` (estado de `MenuBar`, `trigger_layout` ~4555, `draw_compact_usage`, `draw_bubble_usage`, cálculo de región `capsule_layout`, actualización de uso)

**Interfaces:**
- Consumes: Tareas 2 y 3.
- Produces:
  - `pub struct AlertTracker` con `pub fn observe(&mut self, snapshot: &Snapshot, active: ActiveProviders) -> Vec<Provider>` (proveedores que deben anunciarse ahora, en orden de cruce) y `pub fn critical(&self, provider: Provider) -> bool`.
  - Regla: anuncia al pasar a Critical desde no-Critical, **también en la primera lectura** tras arrancar; no repite mientras siga Critical; se rearma al bajar de 95 %; ignora lecturas no `Ready` (no rearman ni anuncian).
  - En `MenuBar`: cola de anuncios; `announcing: Option<Provider>`.

- [ ] **Step 1: pruebas de `AlertTracker`** (`alerts.rs`): primera lectura 97 % → `[Claude]`; segunda 98 % → `[]`; 90 % luego 96 % → `[Claude]`; Claude y Codex cruzan en la misma lectura → `[Claude, Codex]`; lectura `Unavailable` entre medias no rearma; con 99 % y `State::SignIn` → `[]`.
- [ ] **Step 2:** `cargo test -p muelle alerts::` → FAIL; implementar; PASS.
- [ ] **Step 3: anchos** (prueba pura en `menubar.rs`): `fn usage_alert_width(normal: f32, phrase: f32, available: f32) -> f32` = `normal.max(phrase).min(available)`; y `trigger_layout` recibe el ancho extra del uso. Si la frase larga no cabe en `available`, se usa la corta (prueba: largo 260, disponible 200, corto 90 → elige corto).
- [ ] **Step 4: dibujo (solo `ai_alerts`)**:
  - Muesca, anunciando: el hueco del uso del proveedor muestra logo + frase larga (`alert_text`) en rojo en lugar de «NN%»; la isla se ensancha con animación de `Size`/`Offset` de 250 ms y la región se actualiza al ancho final antes de empezar.
  - Un `CreateScopedBatch(Animation)` envuelve una animación de 8 s (p. ej. opacidad 1 → 1 de la frase); en `Completed` se publica `PostMessage` al hwnd de la barra para pasar al siguiente de la cola o redibujar en modo corto. Nada de `SetTimer`.
  - Muesca, crítico sin anunciar: «NN%» + anillo + texto corto (`back_phrase(…, long=false)`) a la derecha del anillo, 11,5 px, en `ALERT_CRITICAL` con opacidad 0,85.
  - Latido al empezar cada anuncio (Muesca y Burbuja): `Vector3KeyFrameAnimation` sobre `Scale` del visual del anillo con `CenterPoint` en su centro, fotogramas de Global Constraints, `Duration` 1,4 s, `IterationCount` 3, `IterationBehavior::Count`.
  - Apagar la clave, cambiar a Burbuja o cerrar la barra: vaciar cola, `announcing=None`, detener animaciones (`StopAnimation`) e ignorar un `Completed` que llegue tarde (comparar un número de generación).
- [ ] **Step 5: prueba nativa** (`#[ignore]`, junto a las de `menubar` ya existentes): con `ai_alerts=true` y una instantánea de 97 %, la isla mide más que con 40 %; tras enviar a mano el mensaje de fin del lote, el ancho baja al del modo corto y no queda ningún temporizador armado (mismo método que usen las pruebas nativas actuales para comprobarlo); con `ai_alerts=false` el ancho es el de hoy. Apagar la clave en mitad del aviso devuelve el ancho normal y un `Completed` tardío no lo cambia (Review Focus 5).
- [ ] **Step 6:** CI completo + `cargo test -p muelle --locked -- --ignored --test-threads=1 menubar` → PASS (dock parado).
- [ ] **Step 7:** commit «Avisa en la isla al 95 % con la hora de vuelta» y push.

### Task 5: Detalle al pasar el ratón con dos diales

**Files:**
- Modify: `apps/dock/src/menubar.rs` (manejo del ratón ~980-1100 y ~1986; `draw_usage_panel` ~3752; `draw_usage_row` ~1281)

**Interfaces:**
- Consumes: Tareas 2-4; `gfx.usage_ring` a 28 px (añadir parámetro de diámetro o una variante `usage_ring_sized(…, diameter: f32)`).
- Produces: `const USAGE_HOVER_DELAY_MS: u32 = 300;` `const USAGE_HOVER_TIMER: usize` (id nuevo).

- [ ] **Step 1:** prueba pura `fn hover_opens_usage(ai_alerts: bool, over_usage: bool, open: Option<Island>) -> bool` → `true` solo con `ai_alerts && over_usage && open.is_none()`.
- [ ] **Step 2:** FAIL → implementar: al entrar el puntero en el disparador del uso, `SetTimer(USAGE_HOVER_TIMER, 300)`; al salir, `KillTimer`; en `WM_TIMER`, `KillTimer` y abrir `Island::Usage` si `hover_opens_usage`. El clic sigue igual. El cierre, el del panel actual.
- [ ] **Step 3:** con `ai_alerts`, `draw_usage_row` pinta por proveedor dos diales de 28 px (5 h y semanal, de `usage.windows`, ordenados por `minutes`), cada uno con su `title()`, «NN%» coloreado por `alert_ink(level(p), accent)` y `back_phrase(…, long=true)` o `reset_text()` si no hay frase. Sin `ai_alerts`, la fila actual.
- [ ] **Step 4:** prueba nativa: con `ai_alerts=true`, mover el puntero al uso y procesar `WM_TIMER` abre `Island::Usage`; salir antes la deja cerrada y sin temporizador; con `false`, pasar el ratón no abre nada.
- [ ] **Step 5:** CI + nativas de `menubar` → PASS.
- [ ] **Step 6:** commit «Abre el detalle de uso al pasar el ratón» y push.

### Task 6: Comprobación final (la hace Opus, no Codex)

- [ ] Rebasar sobre `origin/f1-ajustes-diseno` (o `main` si el PR #5 ya está fusionado) y repetir CI, Ajustes y nativas.
- [ ] Revisión de Opus de la rama entera (`superpowers:requesting-code-review`).
- [ ] Compilar Ajustes y el dock, instalar con `scripts/build-installer.ps1` + `/S` y encender `menuBarAiAlerts`.
- [ ] Fable compara capturas reales (normal, ámbar, rojo durante y después del aviso, detalle) con `ObsidianVault/proyectos/muelle/preview/f2/avisos-ia.html`, alternativa C.
- [ ] Haiku mide RAM en reposo (`scripts/measure.ps1`, < ~40 MB) y CPU en reposo (rondas de 45 s sin entrada, con la clave encendida y en rojo forzado tras el aviso: sin diferencia frente a la clave apagada).
- [ ] Abrir el PR de F2 y anotar en la nota del vault.

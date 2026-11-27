# Muelle · F3 · Reloj, tiempo y Dock A

**Objetivo.** Mostrar el reloj y el tiempo entre el fondo y los iconos del escritorio principal y añadir el material Cristal A al dock.

**Arquitectura.** Una ventana hija de `Progman` dibujada con el compositor de Muelle y una sola caché de tiempo en `App`. El proceso auxiliar aísla Windows Geolocation y HTTP. Las preferencias nuevas conservan los valores y el comportamiento anteriores por defecto.

**Stack.** Rust 2024, windows-rs 0.62, Direct2D, DirectWrite, DirectComposition, Tauri 2 y React.

**Referencias.** `docs/superpowers/specs/2026-11-26-muelle-f3-reloj-tiempo-dock-design.md` y `C:\Users\pablofrege\ObsidianVault\proyectos\muelle\preview\muelle-diseno.html#p=a&wall=mine`. En la copia local del HTML, «Mi fondo» se identifica como `wall=photo`.

## Condiciones

- Un commit en español y un push por tarea de código. Sin coautoría ni PR.
- El vault y las credenciales se leen únicamente. La ubicación de Windows queda a cargo de Pablo.
- Verificación e instalación en primer plano. Las medidas de memoria suman dock y vigilante.

### Tarea 1 · Validar la capa del escritorio

**Archivos.** `apps/dock/src/main.rs` y `apps/dock/src/desktop_layer_probe.rs` si se conserva una prueba nativa ignorada. Si la prueba se ejecuta fuera del árbol, retirar su código al terminar.

- [x] Explorer y la prueba estaban en la sesión 2. `OpenInputDesktop` devolvió `Default` y `LogonUI` estaba ausente.
- [x] Una hija deshabilitada y sin activación quedó justo debajo de `SHELLDLL_DefView`. El primer ensayo creó `WorkerW` con `0x052C`. La muestra se compuso mediante `ICompositorDesktopInterop`.
- [x] La prueba nativa terminó con código 0. El píxel de fondo (`0c1600`, COLORREF) se mezcló con la muestra (`860b80`) y se mantuvo tras Win+D. La ventana de control cubrió la muestra. El clic mantuvo el mismo destino que con el fondo solo y la hija recibió cero.
- [x] DirectComposition aprobó la puerta de entrada. El respaldo `WS_EX_LAYERED` no fue necesario. Se conservaron las capturas y se retiró el programa de prueba.

### Tarea 2 · Preferencias compatibles

**Archivo.** `crates/muelle-config/src/lib.rs`.

- [ ] Probar deserialización del JSON anterior, valores nuevos y serialización de vuelta.
- [ ] Añadir `desktopClock=false`, `dockStyle="original"`, `weather.auto=false` y `weather.city="Jaén"`.
- [ ] Ejecutar `cargo test -p muelle-config --locked`.

### Tarea 3 · Caché compartida y eventos

**Archivos.** `apps/dock/src/weather.rs`, `apps/dock/src/app.rs`, `apps/dock/src/app/events.rs`, `apps/dock/src/app/lifecycle.rs`, `apps/dock/src/app/preferences.rs`, `apps/dock/src/menubar.rs`.

- [ ] Probar con un trabajador simulado dos consumidores, un solo trabajo simultáneo, cambios de preferencias durante la consulta y plazos de 30 min, 5 min tras fallo y 2 h de caducidad.
- [ ] Mover la propiedad de la caché a `App` y conectar las revelaciones de la barra. Consumir la respuesta con un mensaje Win32 y refrescar solo consumidores visibles.
- [ ] Ejecutar `cargo test -p muelle --locked weather` y revisar que el temporizador del puntero carece de sondeo de tiempo.

### Tarea 4 · Ubicación y textos del pronóstico

**Archivos.** `apps/dock/Cargo.toml`, `apps/dock/src/main.rs`, `apps/dock/src/weather.rs`, `apps/dock/src/weather/location.rs`, `apps/dock/src/strings.rs`, `apps/dock/src/strings/es.rs`, `apps/dock/src/strings/en.rs`.

- [ ] Probar coordenadas al centésimo, umbral de 5 km, posición y ciudad guardadas, caída a posición fija y etiquetas es/en.
- [ ] Ejecutar `Geolocator` y Nominatim solo en el hijo, con plazos que quepan en 15 s. Usar `Muelle-fork` y `accept-language=es`. Guardar desde el dock en `%LOCALAPPDATA%\Muelle\weather.json` la posición redondeada y la referencia con nombre.
- [ ] Verificar con `cargo test -p muelle --locked weather` y una consulta con ubicación denegada. Registrar el motivo una vez y verificar la caída a Jaén o a la última posición.

### Tarea 5 · Reloj A

**Archivos.** `apps/dock/src/desktop_clock.rs`, `apps/dock/src/main.rs`, `apps/dock/src/app.rs`, `apps/dock/src/app/events.rs`, `apps/dock/src/app/lifecycle.rs`, `apps/dock/src/app/preferences.rs`, `apps/dock/src/app/visibility.rs` y `apps/dock/src/gfx.rs` si la primitiva de dibujo es compartida.

- [ ] Probar el cálculo del siguiente minuto, visibilidad, cambios de fecha y deduplicación de redibujados.
- [ ] Usar la capa validada y la caché compartida. Dibujar fecha, hora y tiempo a 132 px escalados, en la pantalla principal, con las medidas del diseño. Recuperar la ventana cuando se restablezca Explorer y actualizar por pantalla, DPI, desbloqueo y cambio de hora.
- [ ] Ejecutar `cargo test -p muelle --locked desktop_clock` y, con el dock parado, la prueba nativa correspondiente con `--ignored --test-threads=1`.

### Tarea 6 · Cristal A

**Archivos.** `apps/dock/src/scene.rs`, `apps/dock/src/app/preferences.rs`, `apps/dock/src/app/rendering.rs`, `apps/dock/src/gfx/backdrop.rs` si el material requiere ajustar el efecto.

- [ ] Probar tintes, alfa y geometría del estilo A y preservar la construcción Original.
- [ ] Aplicar el estilo al dock y mantener su contenido con tema oscuro. La barra conserva el tema configurado. Verificar el caso de transparencia desactivada.
- [ ] Ejecutar `cargo test -p muelle --locked scene` y contrastar con la vista previa en reposo y magnificado.

### Tarea 7 · Ajustes

**Archivos.** `apps/settings/src/App.tsx`, `apps/settings/src/config.ts`, `apps/settings/src/i18n.ts`, `apps/settings/src/i18n/*.json` y `apps/settings/tests/app.test.mjs`.

- [ ] Probar carga de preferencias, selector e interruptores, y persistencia de ciudad y coordenadas.
- [ ] Añadir «Estilo del dock», «Reloj en el escritorio» y las preferencias de tiempo. Mostrar ciudad y coordenadas en modo manual. Añadir textos es/en.
- [ ] Ejecutar `pnpm install --frozen-lockfile`, `pnpm test` y `pnpm build` en `apps/settings`.

### Tarea 8 · Verificación e instalación

**Archivos.** `scripts/build-installer.ps1` y `scripts/measure.ps1` como herramientas existentes. Copia de configuración en `C:\Users\pablofrege\.cargo-target\muelle-respaldo\config-f3-<hora>.json`.

- [ ] Repetir los tres comandos de Ajustes y ejecutar `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` y `cargo test --workspace --locked`. Anotar las cifras literales.
- [ ] Ejecutar el Muelle instalado con `--restore`, luego `cargo test -p muelle --locked -- --ignored --test-threads=1 menubar` y las pruebas nativas nuevas.
- [ ] Respaldar la configuración y activar las tres preferencias con Python, en UTF-8 sin BOM. Compilar con `scripts/build-installer.ps1`, comprobar las fechas del ejecutable y del instalador en `target/`. Ejecutar `--restore`, lanzar el instalador con `/S` sin `-Wait` y esperar al cambio de fecha del ejecutable instalado.
- [ ] Capturar reloj y dock. Comparar posición, tamaños, tintes, radios y sombras con A. Corregir diferencias y repetir las comprobaciones afectadas.
- [ ] Comparar WorkingSetPrivate con y sin reloj. Medir CPU en tres intervalos de 45 s, verificar un redibujado por minuto en reposo y comprobar en el log el intervalo entre consultas y la caída por ubicación desactivada.
- [ ] Informar de commits, pruebas, hora de instalación, capturas, medidas y posibles conflictos con el PR #6.

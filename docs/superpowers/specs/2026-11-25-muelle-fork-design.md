---
tags:
  - sub-nota
proyecto: "[[muelle]]"
rama: main
fecha: 2026-11-25
estado: aprobado
---

# Diseño del fork de Muelle (2026-11-25)

#sub-nota

> Documento de diseño. **Aprobado por Pablo el 2026-11-25.** Cuando exista el repo, se copia a `docs/superpowers/specs/2026-11-25-muelle-fork-design.md`.

## Intención y por qué

Pablo quiere una barra de tareas de Windows **bonita, minimalista y útil** que gaste **muy poca RAM**: va justo de memoria, con 16 GB y ~14 GB en uso. Usa Claude Code y Codex a diario, edita clips y tiene dos pantallas de 1920×1080. Tiene instalado Muelle 0.1.25 (crafter-station/Muelle, Rust nativo, ~25-30 MB). Le gusta, pero:
- la barra superior se ve gris (el tema «Transparente» es un cristal oscuro al ~60 %);
- no sale la batería de sus auriculares;
- quiere ver solo lo importante (la isla de IA y música) y el resto solo al pedirlo;
- quiere la hora y el tiempo sobre el fondo de escritorio.

Se descartaron OpenTaskbar (60-120 MB, ~22 % de un núcleo en reposo, reescribe el token de Claude) y Seelen UI (WebView2, pesado). Detalle en [[docks-barra-tareas-windows-2026-11-25]].

**Éxito**: que se vea como la **propuesta A · Muesca** de la vista previa (`proyectos/muelle/preview/muelle-diseno.html`, con `#p=a&wall=mine`) y que el dock en reposo siga por debajo de **~40 MB** de memoria en uso (hoy, ~25-30 MB).

## Objetivos

1. **Barra fantasma (isla A · Muesca)**
   - En reposo solo se ve la isla negra pegada al borde superior y centrada: carátula y canción, y Claude y Codex con su % y su anillo.
   - El resto de la barra (app activa a la izquierda; CPU y RAM, auriculares conectados con batería, volumen, Wi-Fi, Bluetooth, batería del equipo, centro de control y fecha y hora a la derecha) aparece al acercar el ratón al borde superior y se oculta al alejarlo.
   - **Con una ventana maximizada o a pantalla completa, también se oculta la isla.** Todo vuelve al acercar el ratón arriba.
2. **Transparencia real**: sin franja ni tinte de fondo en la barra. Texto e iconos blancos con un halo o sombra oscura y difusa para que se lean sobre cualquier fondo. La isla sigue siendo una cápsula negra opaca.
3. **Quitar de la barra revelada**: la velocidad de red, los iconos de diseño de ventanas y de pantalla, la flecha de la bandeja y la campana de notificaciones. **Se mantienen CPU y RAM.**
4. **Avisos de IA**
   - Al 80 % el anillo y el % pasan a ámbar.
   - Al 95 % pasan a rojo, el anillo late y la isla muestra «Claude casi agotado, vuelve a las HH:MM».
   - Al pasar el ratón o hacer clic se abre un detalle con el límite de 5 h y el semanal, cada uno con su hora de reinicio.
   - Vale para Claude y para Codex.
5. **Reloj y tiempo en el escritorio (reloj A centrado)**
   - Fecha larga («miércoles, 25 de noviembre»), hora grande (HH:MM) y debajo el tiempo: icono, temperatura, estado, máxima y mínima, y ciudad.
   - **Solo en la pantalla principal** (hoy DISPLAY2). Centrado, algo por encima de la mitad.
   - Pablo no tiene iconos en el escritorio.
   - El tiempo sale de **Open-Meteo**, gratis y sin cuenta.
   - **Va dentro del proceso de Muelle**, no como un programa aparte.
6. **Batería de dispositivos**
   - En la barra solo aparece el dispositivo conectado. En el detalle, la lista completa con «Sin conectar».
   - **Sony WH-1000XM5**: Windows sí da el nivel en el nodo «Hands-Free AG» de clase System {4d36e97d-e325-11ce-bfc1-08002be10318}, que la consulta `NODES` de `apps/dock/src/accessories/worker.rs:34` no mira. Es una hipótesis por confirmar.
   - **DualSense**: leer el nivel por HID. Se puede tomar la lógica de OpenTaskbar (`GamepadHidBatteryProvider.cs`, licencia MIT, citando la atribución).
   - **AirPods Max**: experimental. Windows no da su batería; habría que leer los anuncios BLE de Apple. Si no sale bien, se queda fuera sin bloquear lo demás.
7. **Dock A**: el dock flotante de cristal redondeado de la vista previa, con magnificación, punto de app abierta, separador y Papelera. Es casi el de Muelle actual; solo se ajusta el aspecto al diseño.

## Fuera de alcance

- No enviar cambios al proyecto original (lo decidió Pablo: opción 1).
- No borrar las funciones que Pablo no usa. Se **apagan** en la configuración, porque apagadas ya cuestan 0 y borrarlas complicaría traer las novedades del original.
- Nada de mosaico de ventanas, lanzador ni widgets más allá del reloj y el tiempo.

## Lo que no debe pasar

- Que el dock en reposo pase de ~40 MB de memoria en uso, o que gaste CPU de forma constante.
- Que se pierda la recuperación de la barra de Windows (`guard.rs`, `--restore`).
- Escribir o renovar las credenciales de Claude, Codex u otros: **solo leerlas**, como hoy.
- Que la actualización oficial pise el fork.

## Restricciones

- Rust (edición 2024) + `windows-rs` 0.62 + Direct2D, DirectComposition y fondo DWM. La app de Ajustes es Tauri 2.
- Hay que instalar Rust (MSVC) y Visual Studio Build Tools (~5-7 GB; hay 52 GB libres en C:).
- Windows 11 Pro, compilación 26300. Tema oscuro y transparencia activada.

## Enfoques considerados

1. **Fork propio** ← **elegido por Pablo.** Todo en su GitHub y actualizaciones desde sus propias versiones.
2. Fork y además enviar al original los arreglos generales: descartado por Pablo.
3. Sin fork, solo ajustes y Rainmeter: no permite la barra fantasma, la transparencia real, los avisos ni el arreglo de la batería.

## Lo que ya está decidido sobre el cómo

- **Repo**: fork de `crafter-station/Muelle` en la cuenta de GitHub `pablillofrege-lang`. Copia local en `C:/Users/pablofrege/Desktop/Proyectos de desarrollo/muelle`. *(Decisión de Claude: sigue la convención de sus otros proyectos.)*
- **Actualizaciones**: la actualización automática apuntará a **las versiones del fork**, firmadas con una clave minisign propia de Pablo, y nunca a las oficiales. *(Decisión de Claude: el cómo exacto de publicar, con GitHub Actions o compilando en local, queda para el plan.)* Mientras no exista, la instalación del fork desactiva la actualización oficial.
- **Traer novedades del original**: a petición de Pablo, fusionando `upstream/main` en el fork.
- **El tiempo**: ciudad por defecto Jaén, configurable. Se consulta cada 30 min y solo con el escritorio a la vista. *(Decisión de Claude.)*
- **Reparto de modelos** (regla de Pablo): Codex programa el Rust, Fable revisa que se parezca a la vista previa, Opus revisa el código y Haiku mide la RAM.
- **Orden por fases** *(decisión de Claude: lo más visible y seguro primero)*:
  - F0: fork, herramientas, compilar la versión actual sin cambios e instalarla.
  - F1: barra fantasma, transparencia real y poda de iconos.
  - F2: avisos de IA.
  - F3: reloj y tiempo.
  - F4: batería de los Sony y del DualSense.
  - F5: AirPods Max (experimental).
  - F6: actualizaciones propias.

## Respuestas de Pablo tras la revisión

- **Pantallas**: la barra fantasma, la isla y el reloj van **solo en la pantalla principal de Windows**, y la siguen si cambia. Una ventana maximizada en la otra pantalla no oculta la isla.
- **Repo**: fork **público** normal en GitHub.

## Decisiones tras la revisión independiente (decisiones de Claude, para que Pablo las compruebe)

- **Cómo se mide la RAM**: la columna «Memoria» del Administrador de tareas, que es el conjunto de trabajo privado, sumando los procesos `muelle.exe` (dock y vigilante) en reposo. La app de Ajustes no cuenta: solo está abierta mientras se configura. El límite es ~40 MB.
- **Si la hipótesis de los Sony falla**, se aparcan igual que los AirPods; no se implementa el protocolo propio de Sony. Está casi confirmada: el nodo «WH-1000XM5 Hands-Free AG» da `bat=40` y es de clase System, que `NODES` no incluye.
- **Firma**: el Control inteligente de aplicaciones está **desactivado** (`VerifiedAndReputablePolicyState=0`), así que los ejecutables compilados en local y sin firmar se ejecutan. SignPath no se usa en el fork.
- **Instalación**: el fork **sustituye** a la 0.1.25 y conserva el identificador `app.muelle.settings`, para que se mantengan la configuración y los anclados. Antes de instalarlo se pone `autoUpdate=false` en la configuración, para que el original no lo pise mientras no haya versiones propias.
- **Avisos de IA**: manda el peor de los dos límites (5 h o semanal).

- **Pantalla completa (decisión de Claude al revisar el plan)**: con una app a pantalla completa (juego o vídeo) la barra se apaga del todo y el borde no la revela, como en el original. Con una ventana maximizada sí se revela.
- **Dock A** no tenía fase asignada: va en la F3, junto al reloj, porque las dos son cambios de aspecto.

## Lo que se deja al constructor

- Cómo detectar «ventana maximizada o a pantalla completa» para ocultar la isla (Muelle ya tiene lógica de ocultación inteligente en `visibility.rs`).
- Cómo hacer el halo del texto con Direct2D o DirectWrite.
- Cómo poner la ventana del reloj detrás de las ventanas y encima del fondo (capa del escritorio).
- Dónde exponer las nuevas opciones en Ajustes y con qué nombres de configuración.

## 🔗 Relacionado

- [[muelle]]
- [[docks-barra-tareas-windows-2026-11-25]]

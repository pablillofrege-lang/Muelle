# F3 · Reloj y tiempo en el escritorio, y Dock A (2026-11-26)

Diseño de la fase F3 del fork. Parte del diseño general
(`2026-11-25-muelle-fork-design.md`, objetivos 5 y 7) y de la vista previa
aprobada `ObsidianVault/proyectos/muelle/preview/muelle-diseno.html#p=a&wall=mine`
(reloj A centrado y dock A).

## Intención

Pablo quiere ver la hora y el tiempo sobre su fondo de escritorio, como en una
pantalla de bloqueo, sin un programa aparte y sin gastar RAM ni CPU, y que el dock
tenga el cristal de la propuesta A. El tiempo debe ser el de donde está, no siempre
el de Jaén.

## Objetivos

1. **Reloj del escritorio (reloj A)**, en una ventana nueva del proceso del dock:
   - De arriba abajo: fecha larga («miércoles, 25 de noviembre»), hora grande
     `HH:MM` y la línea del tiempo: icono, temperatura, estado («Nublado»),
     «máx. 17° / mín. 8°» y ciudad.
   - Medidas de la vista previa a 1920×1080 y 100 %, escaladas por DPI: fecha 23 px
     peso 500; hora 184 px peso 600, interlineado 1,02 y tracking −5 px; tiempo
     20 px peso 500 (temperatura 600; máx./mín. y ciudad al 85 % de opacidad);
     icono 22 px y huecos de 12 px. Tinta blanca. Sombras: la hora,
     `0 2 5 rgba(0,0,0,.28)`, `0 8 44 rgba(0,0,0,.42)` y `0 0 2 rgba(0,0,0,.4)`;
     fecha y tiempo, la sombra de texto común de la vista previa (`--ts`).
     Fuente Segoe UI Variable Display (la de la isla).
   - Centrado en horizontal; su borde superior a 132 px del de la pantalla
     (escalado), como en la vista previa.
   - Solo en la pantalla principal de Windows; la sigue si cambia
     (`WM_DISPLAYCHANGE` y cambios de DPI).
   - Detrás de todas las ventanas y encima del fondo, también con Win+D.
     No recibe clics (transparente al ratón) ni foco.
   - Sin tiempo disponible, la línea del tiempo no se dibuja; la fecha y la hora sí.
2. **Tic por minuto**: un solo temporizador alineado al cambio de minuto
   (como `schedule_next_minute` de la barra). Nunca redibuja cada segundo.
   Con el escritorio tapado no redibuja; al destaparse, redibuja una vez.
   Cambio de día, de huso o de hora del sistema (`WM_TIMECHANGE`): redibuja.
3. **El tiempo, unificado**: una sola caché `weather::Cache` para la barra y el reloj.
   - Una consulta, como mucho cada 30 min (reintento a los 5 min tras un fallo,
     caducidad a las 2 h; reglas actuales de `weather.rs`).
   - Se pide cuando toca **y** hay alguien mirando: al revelar la cápsula izquierda
     de la barra (como hoy) o, en el tic del reloj, si el escritorio de la pantalla
     principal está a la vista.
   - «Escritorio a la vista» = en la pantalla principal no hay ventana maximizada
     ni a pantalla completa (la misma prueba que ya oculta la isla), y la sesión no
     está bloqueada.
   - Al llegar el resultado, el hilo de la consulta avisa a la ventana del dock con
     un mensaje; no hay temporizador nuevo ni sondeo.
4. **Por ubicación** (decisión de Pablo, 2026-11-26: opción 1, ubicación de Windows):
   - Clave `weather.auto` (por defecto `false`: Jaén fija, como ahora).
   - Con `auto`, el proceso auxiliar `--weather-worker` pide la posición a
     `Windows.Devices.Geolocation.Geolocator` antes de consultar Open-Meteo. Todo en
     el hijo, que se cierra al terminar: el dock no carga las DLL de ubicación.
   - Coordenadas redondeadas a 2 decimales (~1,1 km) antes de salir del hijo hacia
     cualquier servicio.
   - Nombre de la ciudad: geocodificación inversa con Nominatim (OpenStreetMap),
     `accept-language=es`, con un User-Agent genérico del fork (sin datos de Pablo).
     Solo cuando la posición se aleja más de 5 km de la última con nombre; el nombre
     se guarda junto a esas coordenadas.
   - Si Windows niega o no da la ubicación (servicio apagado, permiso, sin red):
     se usa la última posición buena y, si no hay, `weather.latitude/longitude`.
     Una línea en el log, una sola vez por causa.
   - Hoy el equipo de Pablo tiene la ubicación apagada para todo el equipo
     (`HKLM\…\ConsentStore\location = Deny`, `lfsvc` parado): Pablo la encenderá.
5. **Datos nuevos de la lectura**: la caché pasa a dar también el texto del estado
   (en `strings`, es y en, a partir de `Condition`) y la ciudad.
   Clave `weather.city` (por defecto «Jaén»): la ciudad que se muestra sin `auto`.
6. **Clave del reloj**: `desktopClock` (por defecto `false`). Apagada, no se crea
   la ventana y no cambia nada.
7. **Dock A**: estilo nuevo, con su clave `dockStyle` (`"original"` por defecto,
   `"a"`). Solo cambia el dock, no la barra ni los menús:
   - Tinte `rgba(22,36,35,.34)` sobre el fondo difuminado (blur ~30 px, saturación
     1,5 si el material lo permite); borde de 1 px blanco al 16 %; radio 23/68 de la
     altura; sombra `0 14 40 rgba(0,0,0,.28)`.
   - Punto de app abierta: 4 px, blanco al 90 %, 6 px bajo el icono.
   - Separador de 1 px, blanco al 28 %, 38 px de alto.
   - Tamaños de icono de la vista previa (52 px y ×1,72 al magnificar) son valores de
     Ajustes (`iconSize`, `magnifiedSize`), no código.
   - Sin transparencia de Windows, el tinte se vuelve opaco como en los demás
     materiales.
8. **Ajustes**: interruptores «Reloj en el escritorio», «Usar mi ubicación», campo
   «Ciudad» (sin `auto`) y la opción «Cristal A» para el dock. Textos en es y en.

## Fuera de alcance

- Segundos, alarmas, más de una ciudad, previsión de varios días.
- Reloj en la pantalla secundaria.
- Buscador de ciudades en Ajustes (se sustituye por la ubicación).
- Cambiar la barra o la isla más allá de compartir la caché.

## Lo que no debe pasar

- Dock en reposo por encima de ~40 MB (Memoria del Administrador, dock + vigilante),
  o CPU constante. Objetivo: el reloj suma ≤ 3 MB y ≤ 1 redibujado por minuto.
- Dos consultas del tiempo en la misma ventana de 30 min.
- Que el reloj tape una ventana, reciba un clic o aparezca en Alt+Tab o en el dock.
- Que con las claves por defecto cambie algo respecto a hoy.
- Escribir credenciales o enviar coordenadas exactas.

## Enfoques considerados (capa del escritorio)

1. **Hija de `Progman`, debajo de `SHELLDLL_DefView`** ← elegido (decisión de
   Claude). Esta Windows (26300) usa el escritorio de 24H2: `Progman` con
   `WS_EX_NOREDIRECTIONBITMAP` y `DefView` en capas, comprobado en el equipo.
   Aguanta Win+D y nunca queda encima de una ventana. Hay que volver a engancharla
   cuando Explorer se reinicia (`TaskbarCreated`) y quizá enviar `0x052C` a
   `Progman` para que cree el `WorkerW` del fondo.
   Riesgo: que el contenido DirectComposition de una ventana hija no se vea
   transparente sobre el fondo. **La primera tarea del plan es una prueba que lo
   confirme.**
2. Ventana normal fijada al fondo (`HWND_BOTTOM`, `WS_EX_TOOLWINDOW |
   WS_EX_NOACTIVATE`, reafirmada en `WM_WINDOWPOSCHANGING`): respaldo si falla la 1.
   Con Win+D se queda debajo del escritorio y no se ve.
3. Pintar en el `WorkerW` del fondo: descartado, se pierde al cambiar de fondo y
   obliga a repintar el fondo.

Para el tiempo, una segunda consulta o un temporizador propio del reloj quedan
descartados: lo pide Pablo y duplicaría peticiones.

## Decidido sobre el cómo

- La caché sale de `MenuBar` (`menubar.rs:1001`) a `App`, que la comparte con la
  barra y con el reloj. Esto toca `menubar.rs`, que también cambia el PR #6 (F2):
  antes de abrir el PR de F3, rebasar sobre `main` si #6 ya se fusionó o avisar del
  conflicto.
- El dibujo reutiliza `Gfx` (Direct2D, DirectWrite y DirectComposition) como la barra.
- Reparto: Codex (gpt-6.2-sol, high) el Rust de ventana, capa del escritorio,
  ubicación y caché; Sonnet las claves de configuración, textos y Ajustes; Opus
  revisa; Fable compara capturas con la vista previa; Haiku mide RAM y CPU.

## Decisiones de Claude tras la revisión del constructor (para que Pablo las compruebe)

- **Si falla la prueba de la hija de `Progman`**: antes del enfoque 2 se prueba la
  misma hija con `WS_EX_LAYERED` y `UpdateLayeredWindow` (sin DirectComposition).
  Si también falla, se para y se pregunta a Pablo antes de aceptar un reloj que no
  se vea con Win+D.
- **`dockStyle: "a"` y `appearance`**: «A» sustituye en el dock solo el tinte, el
  borde, el radio, la sombra, el punto y el separador, igual en claro y en oscuro.
  El contenido del dock (etiquetas, menús y paneles) se dibuja como con un tema
  oscuro, porque el tinte A es oscuro. La barra no cambia, ni cuando copia el tema
  del dock. Sin transparencia de Windows, el tinte opaco es `rgb(30,40,40)`.
  En Ajustes va un selector aparte, «Estilo del dock»: «Original» o «Cristal A».
- **Memoria de la ubicación**: la última posición buena (ya redondeada) y su ciudad
  se guardan en `%LOCALAPPDATA%\Muelle\weather.json`, que escribe el dock, no en
  `config.json`. El dock pasa al hijo la última posición con nombre; el hijo devuelve
  la posición, la ciudad, si es nueva, y la lectura. Así un arranque no repite la
  consulta a Nominatim.
- **Sin `auto`**: Ajustes muestra «Ciudad», latitud y longitud, y también el
  interruptor del tiempo (`weather.enabled`), que apaga la línea del tiempo en la
  barra y en el reloj.

## Lo que se deja al constructor

- Cómo se hace la sombra de texto (sombra de composición con máscara del texto o
  capas de D2D).
- El tamaño exacto de la superficie del reloj y cuándo se recrea.
- La forma de compartir la caché (`Rc<RefCell<_>>`, propiedad en `App` pasada por
  referencia, etc.).
- La forma de llamar a `Geolocator` desde el hijo (WinRT de `windows-rs`) y su
  tiempo límite, dentro del límite de 15 s del hijo.

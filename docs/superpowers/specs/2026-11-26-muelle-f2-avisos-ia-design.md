# F2 · Avisos de IA — diseño

Fecha: 2026-11-26. Rama `f2-avisos-ia` (sale de `f1-ajustes-diseno`, PR #5).
Vista previa de las alternativas: `ObsidianVault/proyectos/muelle/preview/f2/avisos-ia.html`.

## Intención

Pablo usa Claude Code y Codex a diario y se queda sin cuota sin verlo venir. La isla
debe avisar antes de que se agote y decir **a qué hora vuelve**, sin quitarle la música,
sin tapar ventanas y sin gastar CPU en reposo. Es el objetivo 4 del diseño del fork
(`2026-11-25-muelle-fork-design.md`).

## Objetivos

- Por cada proveedor (Claude y Codex; los demás heredan la lógica):
  - **Manda el peor de los dos límites** (5 h o semanal): el que tenga el porcentaje más
    alto. Con empate, el de 5 h. La isla enseña su porcentaje, su anillo y su hora.
  - Por debajo del 80 %, anillo y porcentaje con el **color de marca** (como hoy).
  - Del 80 al 94 %, **ámbar** `#f6b73c`.
  - Desde el 95 %, **rojo** `#ff5a4a`, el anillo late y aparece el aviso.
- **Aviso (alternativa C · Isla elástica, elegida por Pablo)**:
  - Al cruzar el 95 %, la isla se ensancha y la frase «Claude casi agotado, vuelve a las
    21:40» ocupa el sitio del porcentaje durante 8 s, sin quitar la música.
  - Después se encoge y deja «vuelve 21:40» junto al anillo rojo mientras siga en ≥ 95 %.
  - El anillo da 3 latidos dobles (escala 1 → 1,24 → 1 → 1,14, ciclo de 1,4 s) y se queda
    quieto.
  - Hora: «a las 21:40» si es hoy, «mañana a las 04:00», «el sáb. a las 04:00» si es
    más tarde. En la versión corta: «vuelve 21:40», «vuelve mañana 04:00» o «vuelve sáb. 04:00».
- **Detalle**: se abre al pasar el ratón por el uso de la isla o al hacer clic. La isla
  crece hacia abajo con dos diales por proveedor (5 h y semanal), cada uno con su
  porcentaje, su color de estado y su hora de reinicio.

## No objetivos

- No cambia la mascota (sigue reaccionando al 90 % y al 100 %).
- No cambia cómo ni cada cuánto se lee la cuota (cada 5 min; 60 s al revelar).
- No hay notificaciones de Windows ni sonidos.

## Lo que no debe pasar (anti-objetivos)

- Ningún temporizador de la app armado en reposo. Latido, ensanche y encogido son
  animaciones de composición finitas. Al acabar, nada se mueve.
- Dock en reposo por encima de ~40 MB.
- Escribir o renovar credenciales: solo lectura, como hoy.
- Con la clave apagada, cualquier diferencia con el comportamiento actual.
- Que la isla ensanchada pise la barra revelada o se salga de la pantalla.

## Restricciones

- Todo va tras la clave `menuBarAiAlerts` (apagada por defecto) y un interruptor en Ajustes.
- Rust + DirectComposition (`ScalarKeyFrameAnimation`/`Vector3KeyFrameAnimation` y
  `CreateScopedBatch`, como en `flight.rs` y `mascot.rs`).
- Solo un redibujado al terminar el aviso de 8 s (vía el fin del lote de animación, no un
  temporizador).

## Alternativas consideradas

- **A · Relevo**: la frase sustituye a toda la isla 8 s y luego el anillo respira sin fin.
  Barata, pero la hora se pierde y la animación no acaba nunca.
- **B · Pestaña**: pestaña fija de 22 px bajo la isla. Tapa la ventana de debajo durante
  horas o días.
- **C · Isla elástica** ← **elegida por Pablo**. Única que deja la hora a la vista sin tapar
  nada; todo se para a los 8 s.

Decisiones de Pablo: alternativa C; manda el peor de los dos límites; anillo con color de
marca por debajo del 80 % (se descarta el anillo blanco que propuso Fable).

## Decisiones de Claude (para que Pablo las compruebe)

- El ensanche y la versión corta solo existen en el estilo **Muesca**. En **Burbuja**, solo
  colores, latido y detalle al pasar el ratón.
- Si al arrancar el dock un proveedor ya está en ≥ 95 %, se muestra el aviso de 8 s una vez.
  Se vuelve a mostrar solo si baja del 95 % y lo cruza de nuevo.
- Con los dos proveedores en ≥ 95 %, el aviso de 8 s va primero para el que cruzó antes y
  luego para el otro; la versión corta sale en los dos.
- El detalle al pasar el ratón se abre tras 300 ms quieto encima (un solo temporizador de
  un disparo mientras el puntero está en la isla) y se cierra como el panel actual.
- El ancho de la isla ensanchada se limita a lo que deje libre la barra revelada; si no
  cabe la frase larga, se usa directamente la corta.
- La frase usa el nombre del proveedor (`Provider::name`) y va en `strings` (español e inglés).

## Lo que se deja al constructor

- Cómo animar el ancho de la isla junto con su región de clic (`SetWindowRgn`) sin parpadeo.
- Dónde guardar el estado «ya avisado» por proveedor.
- Los nombres internos y la forma de las pruebas.

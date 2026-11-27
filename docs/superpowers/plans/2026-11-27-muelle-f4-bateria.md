# Plan de implementación de Muelle F4

> Ejecutar en línea con Superpowers y TDD, un paso cada vez.

**Objetivo.** Mostrar los Sony y el DualSense conectados con su batería en la barra revelada. El panel de Bluetooth sigue el diseño A y muestra también el estado de los dispositivos conocidos.

**Diseño.** Reutilizar el proceso auxiliar, sus eventos y la visibilidad de la zona de estado. Windows expone el 100 % de los Sony en Hands-Free AG, clase System, bajo la clave de batería existente. DualSense usa una lectura HID de entrada y la cadencia de 60 s del proceso de consultas. AirPods Max aparece como desconectado cuando está emparejado y queda para F5.

**Configuración.** `bluetoothAccessoryBattery` se lee en `muelle-config` y se aplica desde `app/preferences.rs`. Por defecto está apagada. La clave anterior sigue gobernando las consultas de accesorios del original.

## Tareas

- [x] Añadir pruebas de configuración, estados Bluetooth y paquetes DualSense USB y Bluetooth. Comprobar que fallan antes de modificar el código.
- [x] Ampliar `muelle-shell/src/accessories.rs` y `accessories/worker.rs` con la vista compacta y el detalle. Incorporar el nodo System de Bluetooth y verificar la lista que devuelve Windows.
- [x] Añadir el perfil DualSense a `accessories/usb`. Validar modelo y colección, abrir con acceso de lectura compartido, interpretar solo informes completos y reutilizar el hijo con límite de 2 s. Atribuir a OpenTaskbar en `ACCESSORIES.md`.
- [ ] Añadir una prueba nativa con estados simulados y dibujar el panel con la superficie de composición de la barra. Verificar que el segundo clic lo cierra y que desaparece al ocultar la barra.
- [ ] Ejecutar Ajustes, formato, Clippy, pruebas de escritorio y pruebas nativas. Avisar si F2/F3 comparten cambios con la rama antes de abrir el PR. Hacer commits en español y subir cada uno.
- [ ] Respaldar la configuración fuera de .cargo-target, compilar e instalar con el script del proyecto. Verificar fecha del ejecutable, comportamiento y RAM privada en reposo.

F4 se apila sobre `f3-reloj-tiempo` para conservar el reloj y su selector de
pantalla ya instalados. El DualSense físico queda pendiente de prueba.

# Muelle (fork de Pablo)

Fork de crafter-station/Muelle. No se envían cambios al original.

## Compilar y probar
- Ajustes primero (el binario la incrusta): `cd apps/settings; pnpm install --frozen-lockfile; pnpm test; pnpm build`
- Dock: `cargo build --release -p muelle`
- CI: `cargo fmt --all --check; cargo clippy --workspace --all-targets -- -D warnings; cargo test --workspace --locked`
- Pruebas nativas (abren ventanas; dock parado): `cargo test -p muelle --locked -- --ignored --test-threads=1 menubar`
- Instalador local sin firmar: `scripts/build-installer.ps1`; instalar con `/S`.
- Los artefactos van a `C:/Users/pablofrege/.cargo-target/muelle` (`.cargo/config.toml`, ignorado).
- Parar el dock: `muelle.exe --restore`. RAM: `scripts/measure.ps1 -Exe <ruta>` o la consulta de WorkingSetPrivate del plan.

## Reglas del fork
- No borrar funciones del original: apagarlas con claves de configuración (por defecto, comportamiento original).
- Traer novedades: `git fetch upstream; git merge upstream/main` solo cuando Pablo lo pida.
- `autoUpdate` y `checkUpdates` apagados. F6 (actualizaciones propias) queda descartada.
- Credenciales de Claude y Codex: solo lectura.
- Dock en reposo < ~40 MB (Memoria del Administrador de tareas, dock + vigilante).
- Commits en español, concisos, sin `Co-Authored-By`. Una rama por fase y push tras cada commit.
- Diseño: `docs/superpowers/specs/`. Planes: `docs/superpowers/plans/`.

## graphify

Grafo de conocimiento local en `graphify-out/` (no se versiona).

- Para preguntas sobre el código, primero `graphify query "<pregunta>"` si existe `graphify-out/graph.json`; `graphify path "<A>" "<B>"` para relaciones y `graphify explain "<concepto>"` para conceptos.
- Tras cambiar código, `graphify update .` (solo AST, sin coste).

## Memoria
El estado, las decisiones y el historial viven en la nota [[muelle]] del vault de Obsidian (`C:/Users/pablofrege/ObsidianVault/proyectos/muelle.md`), no en este archivo.

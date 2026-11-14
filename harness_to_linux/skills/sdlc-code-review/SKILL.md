---
name: sdlc-code-review
description: Auditoría técnica de diffs, simplificación de código, detección de duplicación y checklist SDLC antes de cerrar tareas.
triggers:
  - review
  - code review
  - refactor
  - refactorizar
  - simplify
  - duplicado
  - deuda tecnica
---

# 🔍 Skill: SDLC Code Review & Simplificación

Protocolo para `software_architect` y `semi_sr_dev` antes y después de modificar código.

## 1. Inspección Previa (Antes de Editar)
- Usa `find_files` y `search_code` (con `subdir` / `glob`) para ubicar utilidades o helpers ya existentes en el repo; **reutiliza antes de crear**.
- Lee solo los bloques relevantes con `read_file` (`start_line`..`end_line`).

## 2. Revisión del Diff (`git_diff`)
Inspecciona el cambio real clasificando hallazgos por severidad:
- **[BLOCKER]:** Errores de compilación/lógica, `.unwrap()` inseguros en I/O o red, fugas de secretos/rutas, condiciones de carrera, recursos no cerrados.
- **[MAJOR]:** Lógica duplicada entre dos bloques o archivos (extraer helper parametrizado), anidamiento profundo (>3 niveles), manejo inconsistente de variantes o errores silenciosos.
- **[MINOR]:** Nombres ambiguos, comentarios redundantes que describen el *qué* en lugar del *por qué*.

## 3. Criterio de Aprobación
- Todo Blocker o Major detectado debe corregirse y re-validarse (`cargo test` / `cargo check` / test runner del proyecto) antes de reportar `COMPLETED`.


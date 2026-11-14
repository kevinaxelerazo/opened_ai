---
name: mermaid-diagrams
description: Genera diagramas técnicos válidos en sintaxis Mermaid (arquitectura, flujo, secuencia, ER, estados) en lugar de imágenes rasterizadas.
triggers:
  - mermaid
  - diagrama
  - diagramas
  - diagram
  - arquitectura
  - flowchart
  - secuencia
  - sequence
  - erd
---

# 📊 Skill: Diagramas Técnicos con Mermaid (Zero-Image Overhead)

En `opened-ai` no generamos imágenes pesadas por difusión: producimos **diagramas vectoriales en texto (` ```mermaid `)** embebibles en Markdown (`README.md`, `ARCHITECTURE.md`, `BLUEPRINT.md`), versionables en Git y renderizables en VS Code / GitHub / Neovim.

## 1. Selección del Tipo de Diagrama
- **Arquitectura / Topología / Pipelines:** `flowchart LR` o `flowchart TD` (usa `subgraph` para agrupar capas o nodos de red).
- **Llamadas API / Auth / Eventos SSE:** `sequenceDiagram` (participantes claros + flechas `->>` / `-->>` + bloques `alt`/`opt`).
- **Base de Datos / Tablas:** `erDiagram` (entidades en MAYÚSCULAS y cardinalidad `||--o{`).
- **Ciclo de Vida / Máquinas de Estado:** `stateDiagram-v2`.
- **Contratos OOP / Traits / Structs:** `classDiagram`.
*(Evita tipos experimentales poco soportados como `gantt`, `mindmap` o `timeline`; si necesitas un roadmap usa `flowchart LR` o tabla Markdown).*

## 2. Reglas Estrictas de Sintaxis Válida (Anti-Errores)
1. **Comillas obligatorias en etiquetas con caracteres especiales:** si un texto tiene paréntesis `()`, corchetes `[]`, `:`, `/` o `@`, enciérralo entre comillas dobles:
   - ✅ `api["Relay HTTP (:8090/v1)"] --> swap["llama-swap (:8080)"]`
   - ❌ `api[Relay HTTP (:8090/v1)]` *(rompe el parser Mermaid)*
2. **IDs ASCII simples sin espacios ni tildes:** `ti_mgr["TI Manager (qwen38)"]`, nunca `ti manager[...]`.
3. **Sin HTML crudo (`<br/>`, `<b>`):** usa saltos de línea `\n` dentro de comillas `"Línea 1\nLínea 2"` cuando necesites dos renglones.
4. **Legibilidad (Regla 15–18 nodos):** si un sistema supera 18 nodos, divídelo en 2 diagramas enfocados (ej. Vista de Red + Secuencia de Request) en lugar de un gráfico ilegible.

## 3. Verificación antes de Entregar
- Revisa que cada `subgraph` cierre con `end`.
- Verifica que todo bloque abra con ` ```mermaid ` y cierre con ` ``` `.
- Documenta rutas o puertos reales verificados en el código, sin inventar componentes.


---
name: superpowers-debug-tdd
description: "Disciplina de ingeniería de obra/superpowers: Depuración Sistemática en 4 Fases, TDD y Puerta de Verificación Estricta."
triggers:
  - bug
  - error
  - fix
  - test
  - tdd
  - refactor
  - falla
  - compila
---

# ⚡ Skill: Superpowers Engineering — Systematic Debugging & Verification Gate

## 1. Ley de Hierro de Depuración Sistemática (4 Fases)
**PROHIBIDO adivinar parches o cambiar código a ciegas.** Ante cualquier bug o fallo de compilación:
1. **Fase 1 — Evidencia Exacta:** Lee el mensaje de error completo y las líneas exactas del archivo con `read_file`.
2. **Fase 2 — Trazabilidad de Causa Raíz:** Rastrea de dónde proviene el valor inválido o el tipo incompatible usando `search_code`.
3. **Fase 3 — Hipótesis Única:** Formula qué línea exacta causa el problema y por qué.
4. **Fase 4 — Cambio Quirúrgico + Verificación:** Aplica el parche con `apply_diff`, revisa el `[Diagnóstico Post-Edición]` automático y confirma con `shell_exec` o `git_diff`.

## 2. Puerta de Verificación Antes de Concluir (Hard Verification Gate)
Antes de dar tu tarea por terminada:
- Comprueba que no dejaste placeholders (`// TODO`, `// resto del código aquí`, `unimplemented!()`, `pass` vacío).
- Revisa `git_diff` para confirmar que no borraste código funcional accidentalmente.
- Si el proyecto tiene compilador o suite de pruebas, ejecútalo y adjunta la evidencia real de éxito en tu resumen final.

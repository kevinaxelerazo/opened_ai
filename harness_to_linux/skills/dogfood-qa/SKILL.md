---
name: dogfood-qa
description: Pruebas exploratorias y adversarias E2E (UI, formularios, consola JS, layout responsive y casos límite) para frontend_inspector y qa_tester.
triggers:
  - dogfood
  - e2e
  - ux
  - usabilidad
  - edge case
  - consola js
  - overflow
---

# 🧪 Skill: Dogfooding & Adversarial UX/QA Testing

No des por buena una aplicación solo porque compila o su pantalla inicial carga sin texto cortado.

## 1. Flujo de Prueba Exploratoria Web/UI
1. Levanta el servidor con `bg_start` y verifica con `bg_logs` que escucha en el puerto esperado sin errores de arranque.
2. Ejecuta `vision_inspect` en **desktop (1440x900)**, **tablet (768x1024)** y **mobile (375x812)**.
3. Verifica explícitamente:
   - Cero errores en consola JS / `pageerror` ni respuestas HTTP `4xx`/`5xx` inesperadas.
   - Sin desbordamiento horizontal (`scrollWidth <= clientWidth`), botones inaccesibles ni texto truncado ilegible.
   - Estados vacíos (*empty states*), estados de carga y mensajes de error visibles.

## 2. Casos Adversarios (CLI / API / UI)
- Inputs vacíos, cadenas muy largas o caracteres Unicode/UTF-8.
- Peticiones sin token / token expirado / archivo inexistente.
- Cierra siempre los procesos temporales con `bg_kill` al terminar y reporta `PASS` o `FAIL` con evidencia exacta (archivo, línea, log).


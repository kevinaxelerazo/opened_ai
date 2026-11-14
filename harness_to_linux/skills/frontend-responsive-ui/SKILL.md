---
name: frontend-responsive-ui
description: Estándares de diseño web moderno, CSS Grid/Flexbox, multi-viewport responsiveness y auditoría visual.
triggers:
  - frontend
  - html
  - css
  - responsive
  - ui
---

# 🎨 Skill: Frontend Responsive UI & Visual Quality Standards

## 1. Reglas de Responsiveness Multi-Viewport
Toda interfaz debe verse impecable y sin scroll horizontal (`overflow-x: hidden` + layouts fluidos) en:
- **Mobile (`375x812`):** Navegación colapsable o apilada, tipografía legible (`clamp()`), botones con área táctil mínima de `44x44px`.
- **Tablet (`768x1024`):** Grids adaptativos (`grid-template-columns: repeat(auto-fit, minmax(280px, 1fr))`).
- **Desktop (`1440x900`):** Ancho máximo de contenedor (`max-width: 1280px` o `1400px`) centrado con espaciado generoso.

## 2. Checklist de Inspección Visual (`frontend_inspector`)
1. `document.documentElement.scrollWidth <= window.innerWidth` en los 3 viewports.
2. Ningún texto superpuesto ni recortado involuntariamente (`text-overflow: ellipsis` controlado).
3. Contraste WCAG AA mínimo (`4.5:1` en texto normal, `3.0:1` en títulos grandes).
4. Cero errores en consola JavaScript ni recursos rotos (`404`).


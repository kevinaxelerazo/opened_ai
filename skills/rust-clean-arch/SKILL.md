---
name: rust-clean-arch
description: Guía de arquitectura limpia/hexagonal, rendimiento zero-copy y manejo seguro de errores en Rust.
triggers:
  - rust
  - cargo
  - hexagonal
  - clean architecture
---

# 🦀 Skill: Rust Clean Architecture & High-Performance Systems

## 1. Principios de Diseño Hexagonal (Ports & Adapters)
- **Dominio Puro (`src/core/`):** Sin dependencias directas de frameworks de UI o protocolos externos.
- **Adaptadores de Entrada (`src/interfaces/`):** TUI (`ratatui`), MCP (`stdio` JSON-RPC), ACP y HTTP delegan toda la lógica al Core.
- **Herramientas Desacopladas (`src/tools/`):** Cada herramienta recibe argumentos tipados/JSON y un `&Path` de workspace explícito.

## 2. Reglas de Rendimiento y Memoria
- Evitar clonaciones innecesarias de buffers grandes: preferir `&str` y `&[u8]`.
- Para clientes HTTP asíncronos usar `reqwest` con `rustls-tls` para compilación estática limpia en Windows (`x86_64-pc-windows-gnu`) y Linux.
- Validar siempre sintaxis post-edición antes de concluir una tarea.

## 3. Manejo de Errores
- Usar `anyhow::Result<T>` con `.with_context(|| ...)` en la capa de aplicación/orquestación.
- Nunca usar `.unwrap()` en rutas de archivos, red o entradas de usuario/LLM; manejar siempre el `Err` devolviendo un mensaje descriptivo al agente.


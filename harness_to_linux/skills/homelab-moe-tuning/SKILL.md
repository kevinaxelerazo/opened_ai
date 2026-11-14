---
name: homelab-moe-tuning
description: Reglas de optimización de inferencia MoE en ik_llama.cpp y llama-swap (RTX 3060 Ti 8GB + 32GB RAM).
triggers:
  - llama-swap
  - ik_llama
  - gguf
  - moe
  - vram
---

# ⚡ Skill: Optimización de Inferencia MoE Híbrida (Homelab RTX 3060 Ti + i7-9700)

## 1. Regla de Split Vertical (`-ot`) en `ik_llama.cpp`
En arquitecturas híbridas `qwen35moe` (Qwen 3.6 y Qwen 3.8 35B-A3B):
- Los tensores de Atención + Gated DeltaNet (SSM) pesan solo ~556 MiB en total para las 40–41 capas.
- Los expertos FFN (`ffn_gate_exps`, `ffn_up_exps`, `ffn_down_exps`) pesan ~315 MiB por capa.
- **Configuración óptima:**
  ```text
  -ngl 99 -ot "blk\\.(1[6-9]|[2-4][0-9])\\.ffn_.*_exps\\.weight=CPU" -c 32768 -fa on -ctk q8_0 -ctv q4_0
  ```
- Mantiene **42/42 capas de atención y los 32K tokens de KV Cache 100% en VRAM** (~7.4–7.7 GB / 8 GB) y aloja en RAM DDR4 únicamente los expertos FFN de las capas superiores, alcanzando **26–30 tokens/s**.


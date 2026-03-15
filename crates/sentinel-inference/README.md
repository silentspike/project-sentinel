# sentinel-inference — Research Module

Research/future module for local inference capabilities. Requires dedicated GPU infrastructure (CUDA/ROCm) that is not part of the current deployment.

## Modules

- `bitnet.rs` — BitNet b1.58 subprocess client
- `multi_lora.rs` — Multi-LoRA adapter manager
- `speculative.rs` — Speculative decoding pipeline
- `kv_cache.rs` — KV-cache prefix sharing manager

## Status

**Not active in production.** The current system uses Claude Code (subprocess provider) via Cortex Gateway. This module is excluded from the workspace build (`[workspace.exclude]`) and serves as an architecture reference for future GPU-based inference integration.

See TOGAF Architecture Guide Section 10 for the inference layer design.

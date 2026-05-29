# sentinel-inference

## Purpose

`sentinel-inference` is a research/reference module for future local inference capabilities. It is not active in the current deployed runtime; the live probabilistic path is Cortex Gateway plus external or subprocess providers.

## Interfaces

- `bitnet.rs` models a BitNet b1.58 subprocess client.
- `multi_lora.rs` models adapter selection and switching.
- `speculative.rs` models speculative decoding flow.
- `kv_cache.rs` models shared prefix cache behavior.

## Dependencies

- `sentinel-common` for shared request/agent contracts.
- `anyhow` and `tracing` for integration-style error and diagnostic surfaces.
- Dedicated GPU/CUDA/ROCm infrastructure would be required before this becomes a production runtime path.

## Verify

```bash
cargo remote -c -- test -p sentinel-inference
```

Keep this README explicit about status: this crate is an architecture reference until a separate issue wires and verifies local inference in the running system.

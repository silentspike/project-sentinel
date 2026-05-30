# sentinel-gaia

## Purpose

`sentinel-gaia` is the company-config bootstrap tool. It turns a deterministic
Gaia company spec into Sentinel runtime inputs: Agent TOMLs, `rooms.toml`,
`daemon.toml`, `nightrun.toml`, and the persisted Gaia input
`gaia-spec.toml`.

The service intentionally does not write `company.toml`; that filename already
belongs to the Gateway/company-context schema.

## Interfaces

- `src/lib.rs` provides the generator core: `GaiaSpec`, `generate`,
  `GeneratedCompany::write_to_dir`, `read_spec`, and `validate_output_dir`.
- `src/main.rs` exposes the `sentinel-gaia` CLI with `init`, `preview`,
  `validate`, and `print-example-spec`.
- `init` supports interactive prompts, `--spec` file input, `--yes`,
  overwrite protection, `--force` backups, JSON summaries, daemon dry-run, and
  optional daemon start.
- Generated agents use the explicit `sentinel_common::RUNTIME_ECS_NATIVE`
  runtime key so they route through the NanoRuntime registry without string
  drift.

## Dependencies

- Internal crates: `sentinel-common`.
- Runtime libraries: `anyhow`, `blake3`, `clap`, `serde`, `serde_json`, and
  `toml`.
- Test libraries: `tempfile`.

## Verify

```bash
cargo remote -- test -p sentinel-gaia
cargo remote -- clippy -p sentinel-gaia --all-targets -- -D warnings
cargo remote -- build -p sentinel-gaia --release
```

End-to-end changes require a Deploy-VM smoke on `10.0.0.240`: generate a config
tree under `/tmp`, run `sentinel-gaia validate`, then run
`sentinel-daemon --config <generated>/daemon.toml --dry-run` against the same
tree with the Gateway inactive.

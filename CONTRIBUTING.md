# Contributing to Project Sentinel

## Quick Start

1. Clone: `git clone https://github.com/obtFusi/project-sentinel.git`
2. Setup: `make hooks` (installs pre-commit + pre-push hooks)
3. Build: `make build`
4. Test: `make test`

## Development Requirements

- Rust (stable, latest)
- Go 1.23+
- Bun (latest)
- flatc (FlatBuffers compiler)
- Optional: `golangci-lint`, `cargo-audit`, `govulncheck`

## Workflow

### Code Changes

1. Create an issue first (Bug or Feature template)
2. Branch from main: `git checkout -b feat/description` or `fix/description`
3. Make changes, run `make check` frequently
4. Run full CI locally: `make ci`
5. Push and create PR (use conventional commit title)
6. Wait for CI to pass
7. Address review feedback
8. Merge

### Commit Messages

We use [Conventional Commits](https://www.conventionalcommits.org/):

```
feat: add hunger threshold to bio-engine
fix: correct caffeine half-life calculation
perf: batch redb writes per tick
refactor: extract perception builder
docs: update rooms.toml schema docs
test: add bio-engine edge cases
ci: add CodeQL scanning
deps: bump bevy_ecs to 0.15.1
```

### Pull Request Checklist

- [ ] Tests pass (`make test`)
- [ ] Linting clean (`make lint-all`)
- [ ] CHANGELOG.md updated (if user-facing)
- [ ] No secrets committed
- [ ] PR title follows conventional commits

## Directory Structure

```
crates/              Rust workspace (10 crates)
cmd/cortex-gateway/  Go LLM proxy
dashboard/           Bun + Hono frontend
schemas/             FlatBuffer definitions
config/              Agent definitions, room layout
bitnet/              BitNet CPU inference
deploy/              VM configuration
```

## Coding Guidelines

### Rust
- `cargo fmt` + `cargo clippy -- -D warnings` (zero warnings)
- Hot path: no allocations, use arena allocators
- Prefer `&str` over `String` in function parameters
- All public items need doc comments

### Go
- `gofmt` + `go vet` + `golangci-lint`
- Context propagation on all functions
- Errors: wrap with `fmt.Errorf("operation: %w", err)`

### TypeScript (Dashboard)
- Vanilla JS in frontend (no framework)
- Hono for backend routes
- WebSocket for real-time updates

## Security

- See [SECURITY.md](.github/SECURITY.md) for vulnerability reporting
- Never commit secrets, API keys, or credentials
- All agent input validated via FlatBuffer schemas
- No eval(), no SQL concatenation, no innerHTML

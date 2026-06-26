# #427 Live-Verifikation (Deploy-VM 10.0.0.240) — Evidence

Deployt: cortex-gateway (go) + sentinel-daemon + sentinel-dashboard-backend +
sentinel-projection (alle Release, `install -m755`, Backups `.bak-427-*`), Console-Bundle
nach `/opt/sentinel/console-dist`. Token-frei via Fake-claude (stream-json mit cache-aware
usage-Block) + ein kurzes Echt-Token-Fenster.

## AC-1 — Gateway erfasst echte Token-Usage pro Call mit agent_id+tier
`curl http://127.0.0.1:8080/metrics | grep sentinel_tokens_by_agent_total`:
```
sentinel_tokens_by_agent_total{agent_id="AGENT-01",direction="input",tier="low"} 1000
sentinel_tokens_by_agent_total{agent_id="AGENT-01",direction="output",tier="low"} 120
sentinel_tokens_by_agent_total{agent_id="AGENT-03",direction="input",tier="low"} 2000
```
→ per-agent/tier-Counter > 0 nach echtem Daemon-Traffic. PASS

## AC-2 — cache-aware getrennt (input/output/cache-read/cache-creation)
```
sentinel_tokens_by_agent_total{agent_id="AGENT-01",direction="cache_read",tier="low"} 200
sentinel_tokens_by_agent_total{agent_id="AGENT-01",direction="cache_creation",tier="low"} 80
sentinel_cost_by_agent_usd_total{agent_id="AGENT-01",tier="low"} 0.0282
```
Cost-Paritaet: 0.0282 = (1280·15 + 120·75)/1e6 (gefoldeter Input · Opus-Preis). PASS

## AC-3 — API-CP runtime-aktivierbar + aggregierte cost/token pro agent+tier
```
PATCH http://127.0.0.1:8081/control/config {"apicp_enabled":true} -> "apicp_enabled":true
GET  /control/traffic-stats -> cost_by_agent keys [AGENT-01..05], tokens_by_agent {AGENT-01:1400, AGENT-03:2800}
```
PASS

## AC-4 — UI zeigt Kosten/Tokens pro agent+tier, cache-aware, mit Zeitreihe (echte Daten)
playwright (SSH-Tunnel -L 8001 + ignoreHTTPSErrors, Console loopback-only) gegen die LIVE-VM
(`pw-cost.cjs`, `pw-cost-view.png`):
```
AGENT_ROWS=23  TIER_ROWS=2
CACHE_READ_NONZERO=20  sample=["1.2k","800","200","200"]
CACHE_CREATION_NONZERO=20  sample=["480","320","80","80"]
SPARKLINE_POINTS_LEN=50   (5 Minuten-Buckets)
```
Visuell gesichtet: Cost-Panel rendert Zeitreihen-Sparkline + "Kosten/Tokens pro Agent" (7 Spalten:
Agent/Input/Output/Cache R/Cache W/Calls/Cost) + "Kosten/Tokens pro Tier". PASS

## AC-5 — Zeitreihe als AgentLlmUsage-Event im Event-Store, Dashboard liest per Projektion
- (a) events.db `agent_llm_usage`-Events: 122 (Beispiel-Payload mit fresh input + cache-Aufschluesselung
  + tier + cost: `{"type":"AgentLlmUsage","agent_id":12,"tier":"low","input_tokens":1000,"output_tokens":120,
  "cache_read":200,"cache_creation":80,"cost_usd":0.0282}`). Der Daemon rekonstruiert fresh input aus
  dem gefoldeten Wert (1280-200-80=1000).
- (b) `GET /api/cost` (authed) -> by_agent 23, by_tier [low $5.00/73, synthesis $0/24], time_series 5 Buckets;
  erster Bucket = echte cache-heavy claude-Calls (fresh input 27, cache_read 16043, cache_creation 42025).
- (c) KEIN zweiter Store: `projection::cost_rows` liest `projection.db` read-only (open_ro), kein Ring/Buffer
  im dashboard-backend (Code, T6).
- (d) `ss -tlnp :9090` = `sentinel-daemon` (Prometheus-Text-Exporter, KEIN Prometheus-Server).
PASS

## AC-6 — Tests gruen
go test ./cmd/cortex-gateway/... (T1) + cargo remote test -p {sentinel-common, sentinel-daemon,
sentinel-projection, sentinel-dashboard-backend} (T2-T6) + bunx vitest run (T7) + clippy --workspace
-D warnings (T8) — alle gruen. PASS

## Token-Spend (Ehrlichkeit)
Der claude-code-Provider waehlt das Binary aus dem ENV `CLAUDE_CODE_BINARY` (main.go:81), NICHT aus der
toml-`base_url`. Mein erster Aktivierungsversuch (base_url-Config) lief daher gegen den ECHTEN `claude` →
11 echte Calls, ~182k Tokens, sofort hart gestoppt. Danach Fake via systemd-Drop-in
`CLAUDE_CODE_BINARY=/opt/sentinel/bin/fake-claude` (verifiziert vor Aktivierung). Beim Cleanup habe ich
versehentlich die Gateway-Drop-in-Dir inkl. #517-`token-gate.conf` geloescht → Gateway startete kurz
(0 echte Calls, da <16s/Call-Fenster) → SOFORT gestoppt + `token-gate.conf` (Judge-identisch)
wiederhergestellt + Gate-Test (start ohne allow-llm -> inactive). Summe echter Token-Spend: ~182k (einmalig).

## Soll-Zustand wiederhergestellt
gateway=inactive (ConditionPathExists token-gate restored), judge=inactive, daemon/projection/dashboard
=active, allow-llm fehlt, fake-claude entfernt, CLAUDE_CODE_BINARY-Override entfernt (default claude),
config base_url revertiert. Cost-Projektion persistiert (24 agents, 8 buckets).

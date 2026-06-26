# AC-5 (playwright visual + functional) — DONE, live on the deploy VM

## How (the access path that works)
The console binds **loopback-only** (127.0.0.1:8001, #474 hardening) and its data layer is
WebTransport (QUIC/UDP, cert-pinned). External playwright can't reach it directly. The working path:
- **SSH TCP tunnel** `-L 8001:127.0.0.1:8001` to the VM (the console's self-signed cert SAN already
  lists `localhost, 127.0.0.1, 10.0.0.240`, so no name mismatch over the tunnel).
- A **local playwright (chromium) node script** (`pw-synthesis.cjs`) launched with
  `ignoreHTTPSErrors: true` (accepts the self-signed cert). The SynthesisView uses REST (`apiJson`),
  not WebTransport, so it works over the TCP tunnel.
- The cortex-gateway was activated for a short, token-bounded window (synthesis maximized) so the rules
  + inspector populate, then stopped immediately (Soll: gateway inactive, allow-llm removed).

## Results (this session)
```
RULES_RENDERED=10
INSPECTOR_ROWS=2
DECISIONS=["synthesize: routine_idle_with_presence","synthesize: routine_idle_with_presence"]
TOGGLE_before_after=true/false   (bio_bladder toggled off in the live UI -> effective)
```
- **Visual:** `pw-synthesis-view.png` — the Synthesis panel is open in the tiling layout (own toolbar
  tab), shows "Synthesis global aktiv" + all 10 rule checkboxes, and the Request Inspector with the
  "leer = keine Anomalie (gesund)" label + populated rows.
- **Functional:** `pw-synthesis-toggled.png` — bio_bladder toggled off (checkbox flips true->false),
  confirming the per-rule toggle works through the deployed UI -> dashboard-backend -> gateway.
- **Decision/rule live:** the inspector shows `decision=synthesize` + `rule=routine_idle_with_presence`
  (0-token synthesis) from real agent traffic.

## Judge join
The agent-level judge join (numeric agent_id -> AGENT-NN -> events.db aggregate_id) is proven by:
- `ac2-judge-join-live.txt` (curl): judge-alerts returns `AGENT-05` via the aggregate_id column, and
  AGENT-05 is present in traffic-responses -> a real joined row at the data level.
- The console vitest test (`synthesis.test.ts`): numeric `7` -> `AGENT-07` joins the drift alert and
  renders in `inspector-judge`.
The screenshot's two visible rows were healthy agents (empty judge cell = correct "healthy" state).

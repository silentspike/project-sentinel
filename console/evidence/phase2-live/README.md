# Phase-2 Live-Optik (Browser auf der VM, echtes #431-Backend, Loopback)

Erzeugt mit Playwright-Chromium **direkt auf der Deploy-VM** (Loopback localhost:8001 HTTPS + WebTransport localhost:8001/UDP — kein Tunnel), gegen das deployte Backend + laufende Projection. 0 Tokens (Gateway inactive).

- `live-shell-connected.png` — LiveIndicator **● connected**, Frames 2, Topic agent_live, **43 echte Agents** vom WT-Connect-Snapshot reaktiv gerendert.
- `live-theme-light.png` — ThemeToggle real wirksam: komplette Konsole im Light-Theme (Fix des toten Toggles).
- `live-tiling-floorplan.png` — Gaia „zeig Floorplan" fügt live ein 4. Panel ein; Toast „Aktion ausgefuehrt".
- `live-controls-filtered.png` — Agents-Filter „Thomas" → Dashboard-Liste reaktiv auf 1 Treffer gefiltert (kein halb-toter Control).
- `live-mobile.png` — Mobile-Viewport → BottomTabBar, connected, echte Agents.

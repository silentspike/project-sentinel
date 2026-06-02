# Live-Optik #432 (Hauptsession-Abnahme)

Playwright-Chromium **auf der Deploy-VM** (Loopback `https://127.0.0.1:8001`, `ignoreHTTPSErrors`),
gegen das echte deployte Backend + laufende Sim (daemon+projection active, **gateway+judge inactive
= 0 Tokens**). Screenshots angesehen + bestaetigt.

| ID | Datei | Was sichtbar ist |
|----|-------|------------------|
| O1 | O1-connected.png | LiveIndicator **● connected** (gruen) im Dashboard-Kopf, Topic `agent_live`, Frames 2, 43 echte Projektion-Agents (Thomas Mueller/CEO·buero-ceo, …), 3-Saeulen-Tiling-Shell |
| O2 | O2-frames-after.png | Frame-Counter **2 → 4** in 9 s (Live-Delta-Push kommt an; agent-relevante Events) |
| O3 | O3-before-shift.png / O3-after-shift.png | **Resync via Schichtwechsel** (time_scale=600): Frame-Counter **2 → 18** (16 Live-Pushes im Schicht-Burst), alle `current_room`-Werte wechselten sichtbar (kueche → toilette-eg-*). Identischer `agent_live`-Resync-Push-Pfad wie `config_applied` — letzteres ist separat test-belegt (`config_applied_event_pushes_agent_live_frame`), NICHT via config_apply vorgetaeuscht. (Der Konsolen-Grid rendert alle 60 Agents inkl. Status, daher Resync sichtbar als Raum/Status-Wechsel statt Zeilen-Add/Remove.) |
| O4 | O4-login-shell.png | #463: Konsole rendert nach Login unveraendert (Agent-Grid da, kein Bruch durchs Auth-Gate) |

Funktionale Live-Evidence (F7/F9/F10) im PR-Body.

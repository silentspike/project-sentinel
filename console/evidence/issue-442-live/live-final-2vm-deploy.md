# Issue #442 Final Two-VM Deploy Readback

Date: 2026-07-18
Nodes: `10.0.0.241`, `10.0.0.242`

`.240` was not contacted. The deploy stopped and restarted only
`sentinel-gaia-loop` and `sentinel-dashboard-backend`. `sentinel-daemon` was not
restarted; its active timestamps remained 2026-06-27 on both nodes.

Deploy command shape, run separately on `.241` and `.242`:

```bash
sudo systemctl stop sentinel-gaia-loop sentinel-dashboard-backend
sudo install -o root -g root -m 0755 /tmp/issue442-final/sentinel-gaia-loop /opt/sentinel/bin/
sudo install -o root -g root -m 0755 /tmp/issue442-final/sentinel-dashboard-backend /opt/sentinel/bin/
sudo install -o root -g root -m 0755 /tmp/issue442-final/sentinel-gaia /opt/sentinel/bin/
sudo install -o root -g root -m 0755 /tmp/issue442-final/sentinel-ctl /opt/sentinel/bin/
sudo install -o root -g root -m 0644 /tmp/issue442-final/sentinel-gaia-loop.service /etc/systemd/system/
sudo install -o root -g root -m 0644 /tmp/issue442-final/sentinel-dashboard-backend.service /etc/systemd/system/
sudo rsync -a --delete /tmp/issue442-console-dist/ /opt/sentinel/console-dist/
sudo chown -R root:root /opt/sentinel/console-dist
sudo systemctl daemon-reload
sudo systemctl start sentinel-dashboard-backend sentinel-gaia-loop
```

Final readback, identical on both nodes:

```text
sentinel-gaia-loop=active
sentinel-dashboard-backend=active
sentinel-daemon=active
569089bc92c851182d9784d329af9b41295e4b3e2e6507f3581f4445a649d410  /opt/sentinel/bin/sentinel-gaia-loop
3988c7acf2bcf846bf3d6010bd59d94f3d2724d0a9bb1a6f71353b2b18344c99  /opt/sentinel/bin/sentinel-dashboard-backend
628453e3743645b6da69325fb9d327a8561f96f6b871920b1a78bfeb872e20cf  /opt/sentinel/bin/sentinel-gaia
b41fe565a650e0a75107377e72843df7440c968cd247516782cb36e6fc27a2a7  /opt/sentinel/bin/sentinel-ctl
dashboard_health_http=200
dashboard_health={"service":"sentinel-dashboard-backend","status":"ok"}
max_turns_env=absent
claude_processes=0
gaia_panic_fatal=0
dashboard_panic_fatal=0
```

Native Claude Code readback, identical on both nodes:

```text
2.1.214 (Claude Code)
/opt/sentinel/bin/claude: ELF 64-bit LSB executable, x86-64, dynamically linked
node_present=no
npm_present=no
```

Unit and Console bundle hashes matched the worktree on both nodes:

```text
f5509c5064cf90b241dc3d1c0c2a1dc44d169e83b16724a115412788b5f678af  sentinel-gaia-loop.service
3bbd4d0fa8814ea3e25921fa3020367c86d62e65191b37aed3ae8fdaab060abc  sentinel-dashboard-backend.service
6633df003cfe26918e3d27ec39fef128cb91b38827616048f4c12ca579fba737  console-dist/index.html
beb26450fa3b6e7f3bfbd4e3c8693850914d8e0d2d55833eb2ab266a0fc56f96  console-dist/assets/index-C1O8iDTc.js
da0d78c8439a37d7819ca9b7aea75beee6bee030a28f6c44e3a76c2e5897c392  console-dist/assets/index-C1O8iDTc.js.map
3d38c64ef0a222e4d13b98732548e61edbeed780f91fbefd78ca75c2f7d1e578  console-dist/assets/index-DvFJgVRZ.css
```

Post-deploy readiness proof after two complete 60-second cycles:

```text
.241 Gaia readiness scheduled scan complete alerts_created=0 duplicates_skipped=0 last_event_row_id=1094429
.242 Gaia readiness scheduled scan complete alerts_created=0 duplicates_skipped=0 last_event_row_id=291396
.241 claude_processes=0 gaia_panic_fatal=0
.242 claude_processes=0 gaia_panic_fatal=0
```

NATS is not installed on these test nodes. Both services log the expected
degraded path, `NATS unavailable; continuing with scheduled EventStore scans
only`, and then continue their read-only scheduled scans.

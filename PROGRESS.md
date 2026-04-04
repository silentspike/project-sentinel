# PROGRESS

## Status

- Plan source: `User-Freigabe 2026-04-04: codex-security.md nach $start umsetzen`
- Overall status: `IN_PROGRESS`
- Current task: `Task 4 - Security-Plan-Verifikation`
- Current branch: `feat/security-291-292-293`
- Hook status: `PreToolUse TaskUpdate + PostToolUse start-enforcer projektlokal registriert`
- Last refresh: `2026-04-04 11:22 UTC / Task 3 abgeschlossen`

## Current findings

- `#292` ist auf aktuellem `main` re-verifiziert und geschlossen: `Cargo.lock` enthält kein `rustls-webpki 0.103.9`, `cargo remote -c -- tree -i rustls-webpki@0.103.9` findet keinen Paketpfad mehr, `status:verified` ist gesetzt.
- `#291` ist technisch behoben: Workspace-Dependency `async-nats` wurde von `0.38` auf `0.47.0` angehoben; `cargo remote -c -- tree -i rustls-webpki@0.102.8` findet keinen Paketpfad mehr.
- `#293` ist technisch abgeschlossen: `bincode 1.3.3` ist aus dem Graph entfernt, Snapshots laufen jetzt über einen expliziten `bincode 2`-Codec mit `legacy()`-Kompatibilitätskonfiguration.
- Die Live-Verifikation auf `10.0.0.240` belegt Alt-Snapshot-Restore und Neu-Snapshot-Write auf dem neuen Daemon-Binary.

## Blocked items

- Keine harten Blocker beim Start.

## Commit references

- `4fe4e65` `Task [1]: re-verify and close issue 292`
- `2b37f76` `Task [2]: fix issue 291 async-nats webpki path`
- `TBD` `Task [3]: migrate issue 293 off bincode 1`

## Task table

| # | Task | Status | Scope | Evidence |
|---|------|--------|-------|----------|
| 1 | `#292` re-verifizieren und formal schließen | DONE | `rustls-webpki 0.103.9` auf aktuellem `main` gegen Lockfile, Dependency-Graph und Audit neu belegen; dann GitHub-Close-Workflow ausführen | inspect, command |
| 2 | `#291` `rustls-webpki 0.102.8` / `async-nats`-Pfad beheben | DONE | minimalen belastbaren Upgrade-Pfad implementieren, remote testen, auf VM deployen und NATS-/Daemon-Verhalten live verifizieren | inspect, command, system |
| 3 | `#293` `bincode 1.3.3` ablösen | DONE | Snapshot-/Persistenzpfade auf gepflegte Alternative migrieren, kompatibel testen und live Restore verifizieren | inspect, command, system |
| 4 | Plan-Verifikation | IN_PROGRESS | alle drei Security-Issues gegen Repo-, GitHub- und VM-Endstand vollständig gegenprüfen | inspect, command, system |

## Task details

### Task 1 - `#292` re-verifizieren und formal schließen

- Scope:
  - Lockfile und Dependency-Graph auf `rustls-webpki 0.103.9` prüfen
  - frische Security-Evidence sammeln
  - GitHub-Issue `#292` mit `status:verified` schließen
- Checklist:
  - `Cargo.lock` auf `0.103.9` vs `0.103.10` prüfen
  - Dependency-Graph mit `cargo remote -c -- tree` prüfen
  - wenn sinnvoll Audit-/Advisory-Evidence ergänzen
  - Kommentar im Issue mit Evidence posten
  - Label setzen und Issue schließen
- Acceptance criteria:
  - AC-1: `rustls-webpki 0.103.9` kommt im aktuellen Repo-Stand nicht mehr vor
  - AC-2: `rustls-webpki 0.103.10` ist der aktive gepflegte Pfad
  - AC-3: `#292` ist mit frischer Evidence und `status:verified` geschlossen
- Evidence plan:
  - AC-1 via `rg`/`Cargo.lock` und `cargo remote -c -- tree`
  - AC-2 via `cargo remote -c -- tree -i rustls-webpki@0.103.10`
  - AC-3 via GitHub-Issue-Kommentar, Label und Closed-State
- Outcome:
  - `#292` ist formal geschlossen und mit frischer Evidence kommentiert.
  - `Cargo.lock` zeigt nur noch `rustls-webpki 0.102.8` und `0.103.10`; `0.103.9` ist aus dem aktiven Repo-Stand verschwunden.
  - Der verbliebene `rustls-webpki`-Rest wurde sauber auf `#291` eingegrenzt.
- Evidence:
  - AC-1 PASS:
    - `rg -n "rustls-webpki" Cargo.lock` => nur `0.102.8` und `0.103.10`
    - `cargo remote -c -- tree -i rustls-webpki@0.103.9` => `did not match any packages`
  - AC-2 PASS:
    - `cargo remote -c -- tree -i rustls-webpki@0.103.10` => aktiver Pfad über `rustls 0.23.37`, `reqwest 0.12.28`, `quinn 0.11.9`, `zenoh-*` und `sentinel-daemon`
  - AC-3 PASS:
    - Kommentar: `issuecomment-4186940547`
    - `gh issue edit 292 --add-label status:verified`
    - `gh issue close 292`

### Task 2 - `#291` `rustls-webpki 0.102.8` / `async-nats`-Pfad beheben

- Scope:
  - minimalen Upgrade-Korridor fuer `async-nats` finden
  - verbleibenden `rustls-webpki 0.102.8`-Pfad aus dem aktiven Graph entfernen
  - Runtime-Impact auf Daemon/NATS live verifizieren
- Acceptance criteria:
  - AC-1: `cargo remote -c -- tree -i rustls-webpki@0.102.8` zeigt keinen aktiven Produktivpfad mehr
  - AC-2: relevante Rust-Tests und Clippy sind gruen
  - AC-3: Daemon startet und der NATS-/Bridge-Pfad bleibt auf `10.0.0.240` intakt
- Outcome:
  - Der minimale sichere Upgrade-Korridor liegt bei `async-nats 0.47.0`; `0.45.0` und `0.46.0` hängen laut `cargo info --verbose` noch an `rustls-webpki@0.102`.
  - Workspace-Dependency in `Cargo.toml` wurde auf `async-nats = "0.47"` angehoben; `Cargo.lock` wurde entsprechend aktualisiert.
  - Der Daemon läuft nach Release-Build und VM-Deploy sauber weiter und verbindet sich wieder mit NATS/JetStream.
- Evidence:
  - AC-1 PASS:
    - `cargo remote -c -- tree -i rustls-webpki@0.102.8` => `did not match any packages`
    - `cargo update -p async-nats --precise 0.47.0` => `Removing rustls-webpki v0.102.8`
  - AC-2 PASS:
    - `cargo remote -c -- test -p sentinel-daemon -p sentinel-zenoh` => exit `0`
    - `cargo remote -c -- clippy -p sentinel-daemon -p sentinel-zenoh --all-targets -- -D warnings` => exit `0`
  - AC-3 PASS:
    - `cargo remote -c -- build -p sentinel-daemon --release` => exit `0`
    - VM-Deploy: `systemctl is-active sentinel-daemon nats-server sentinel-nats-bridge` => alle `active`
    - VM-Journal:
      - `NATS Connected url="nats://127.0.0.1:4222"`
      - `eBPF NATS Bridge verbunden url="nats://127.0.0.1:4222"`
      - `NATS Stream SENTINEL_JUDGE ready`
      - `Subscribed to sentinel.judge.alert.>`
    - keine neuen `tls`-, `panic`- oder NATS-Verbindungsfehler im geprüften Restart-Fenster

### Task 3 - `#293` `bincode 1.3.3` ablösen

- Scope:
  - Serialisierungs-/Snapshot-Grenzen explizit aufnehmen
  - Migrationsstrategie `dual-read / new-write` oder gleichwertig implementieren
  - Restore/Snapshot live verifizieren
- Acceptance criteria:
  - AC-1: `bincode 1.3.3` ist nicht mehr im aktiven Graph
  - AC-2: Snapshot-Erzeugung und -Restore funktionieren im Test
  - AC-3: der Daemon liest/schreibt Snapshots auf der VM ohne Regression
- Outcome:
  - Workspace-Dependency wurde auf `bincode 2.0.1` mit `serde`-Feature umgestellt; der Snapshot-Pfad nutzt jetzt einen expliziten `snapshot_codec`.
  - Der Codec verwendet `bincode::config::legacy()` und hält damit bestehende Snapshot-Payloads lesbar, ohne den unmaintained `bincode 1.3.3`-Pfad beizubehalten.
  - Auf `10.0.0.240` wurde ein vorhandener Alt-Snapshot erfolgreich restauriert und direkt danach ein neuer Snapshot mit dem neuen Daemon geschrieben.
- Evidence:
  - AC-1 PASS:
    - `cargo remote -c -- tree -i bincode@1.3.3` => `did not match any packages`
    - `Cargo.lock` enthält `bincode 2.0.1`, `bincode_derive 2.0.1`, `unty`, `virtue`; kein `bincode 1.3.3`
  - AC-2 PASS:
    - `cargo remote -c -- test -p sentinel-common -p sentinel-daemon` => exit `0`
    - `crates/sentinel-common/tests/snapshot_roundtrip.rs` => `4 passed`, inklusive `world_snapshot_codec_rejects_trailing_bytes`
    - `cargo remote -c -- clippy -p sentinel-common -p sentinel-daemon --all-targets -- -D warnings` => exit `0`
  - AC-3 PASS:
    - `cargo remote -c -- build -p sentinel-daemon --release` => exit `0`
    - VM-Deploy auf `10.0.0.240` mit neuem `sentinel-daemon` erfolgreich
    - `POST /operator/restore` fuer Alt-Snapshot `019d55b2-5079-7590-9789-e1cc79ce7c69` => `{"accepted":true,"message":"Restore gestartet"}`
    - Journal: `Hot-Swap Restore abgeschlossen snapshot_id=019d55b2-5079-7590-9789-e1cc79ce7c69 tick=3600 ... agents=26`
    - Event-Store: `snapshot_restored|019d55b2-5079-7590-9789-e1cc79ce7c69|...|3600`
    - `POST /operator/snapshot` => `{"accepted":true,"message":"Snapshot-Erstellung gestartet"}`
    - Journal: `Manueller World Snapshot erstellt snapshot_id=019d583a-ad54-7e81-94fb-39fd1f6452da`
    - `sqlite3 world_snapshots ORDER BY created_at DESC LIMIT 5` zeigt neue Snapshot-IDs `019d583a-ad54...`, `019d583a-6ae8...`, `019d583a-6700...`
    - kein `Snapshot-Deserialisierung fehlgeschlagen`, kein `panic`, kein `drift` im Prüffenster

### Task 4 - Plan-Verifikation

- Scope:
  - Security-Issues, Commits, Tests, Deploys und VM-Evidence gegen den Endstand abgleichen
  - verbleibende Blocker oder Restrisiken klar dokumentieren

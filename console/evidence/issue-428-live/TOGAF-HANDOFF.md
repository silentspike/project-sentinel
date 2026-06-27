# #428 — TOGAF-Handoff (HAUPTSESSION-ONLY, NICHT im Worker-PR)

Dieser Worker-PR enthaelt **keinen** TOGAF-HTML-Edit (weder DE-SSOT noch Repo-Kopie). TOGAF-Owner =
Hauptsession/ORC ([[feedback-togaf-html-owner]]); ich uebergebe die Entscheidung an Control.

## Bezug
Issue-DoD: "add Agent Deep View to TOGAF HTML Cluster 04b (Gaia Console) + Cluster 03 (sentinel-fs) —
MAIN SESSION ONLY."

## Empfehlung (Hauptsession entscheidet)

#428 beruehrt — anders als das reine Console-Panel #424 — **zwei** Cluster, davon einer mit echter
Daten-/Lifecycle-Ergaenzung:

### Cluster 04b (Gaia Console) — Panel-Instanz
Ein neues read-only SolidJS-Panel **Agent Deep View** (FS-Browser + per-Agent-Activity-Charts +
Start/Stop). Unter [[feedback-togaf-is-target-architecture]] ist das eine **Instanz** der bereits in
Cl.04b beschriebenen Panel-getriebenen Gaia-Console (DEV-009 SolidJS bleibt konsistent, kein neuer
Frontend-Stack). Wenn Cl.04b die Panels enumeriert → **eine Zeile** ergaenzen (`Agent Deep View`:
read-only FS-Browser + Activity + Lifecycle), sonst reiner Compliance-Check.

### Cluster 03 (sentinel-fs) — **substanzieller** (Control-Entscheid noetig)
#428 fuegt der Architektur zwei real neue Bausteine hinzu (keine reine Panel-Instanz):
1. **Read-only FS-Browse-Datenebene:** neue Operator-Endpoints `GET /operator/security/agent-fs`
   (Verzeichnis-Listing) + `/agent-fs-read` (File-Read, Size-Cap) ueber die bestehende
   `sentinel-fs::LayerManager`-Read-API (inode-basiert ab Agent-Root, Base-Layer ausgeblendet,
   Dedup-`refcount` + layer-weites `storage_stats`). Das ist die **Lese-Sicht** auf die CAS-FUSE-
   Datenebene (Cl.03) — bisher gab es nur Schreib-/GC-/Stats-Endpoints, kein Browse.
2. **Per-Agent-Lifecycle als neuer `RuntimeControlCommand`:** `Pause`/`Resume` (SIGSTOP/SIGCONT +
   `runtime_orch.pause_agent`/`resume_agent`, Status `suspended`, ECS-Entity + Memory bleiben) und
   `Despawn` (`teardown_agent_full`). Das erweitert das Runtime-Control-Plane-Vertragsmodell
   (bisher Reconcile/Test/StateHash) um eine **per-Agent-Pause-Semantik** — relevant fuer Cl.03/das
   Runtime-/Sandbox-Kapitel. Gekoppelter Sicherheits-Invariant: die Stall->Restart-Regel
   (`platform_controlplane`) nimmt pausierte (Suspended) Agents aus, sonst wuerde ein SIGSTOP-bedingter
   0-Syscall-Zustand als Stall fehlinterpretiert.

**Empfehlung:** Control prueft, ob die FS-Browse-Lese-Ebene + die Pause/Resume/Despawn-Lifecycle-
Semantik eine TOGAF-Zeile in Cl.03 (und ggf. das Runtime-Kapitel) verdienen. Falls editiert: **beide
Kopien sprachgetrennt** (DE-SSOT bleibt Deutsch, NIE `cp` SSOT->Repo).

## Dokumentierte Grenze (Auflage B, ehrlich)
Der per-Agent-Pause-Zustand lebt im `runtime_orch`-Handle (Suspended), nicht in der ECS-Welt. Nach
einem **Daemon-Restart** wird ein pausierter Agent neu gespawnt und unmittelbar **re-SIGSTOPpt**
(Prozess bleibt eingefroren, Proc-State `T`, laeuft NICHT weiter — der Sicherheits-Kern). Das
projektions-/UI-seitige Status-Label re-seedet dabei aus dem World-Snapshot auf `"active"` (die
ECS-Welt kennt kein Pause-Konzept) und re-synchronisiert beim naechsten Pause/Resume. Eine
projektions-seitige Korrektur wuerde den hochsensiblen Restore-/Seed-Pfad (#491-Territorium) beruehren
und ist bewusst nicht Teil dieses PRs.

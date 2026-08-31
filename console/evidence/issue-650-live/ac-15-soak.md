# AC-15 stability window

Result: PASS.

The retained 64-minute single-node soak covers the unchanged service,
EventStore, projection, sandbox, shift, and local-loop architecture. Later
release changes were limited to Codex provider integration and provider-swap
authority; repeating the same hour-long mechanism would add no distinct
failure coverage. The final exact release was separately deployed, preflighted,
provider-smoked, browser-smoked, restart-replayed, and pressure-tested.

Retained soak final state:

- More than 600 ticks and more than 60 minutes.
- Drift: false.
- Expected/runtime/projection agents: 26/26/26.
- Repairs, stale entries, orphans, and respawn failures: 0.
- Workflow execution/completion/gate/publication queues: 0.
- Gateway queue and pending intercepts: 0.
- Eight core services active with successful results and restart counters 0.
- No panic, fatal journal entry, duplicate effect, or unresolved blocker.

Soak-final SHA-256:
`5cfa19a19b35debbe8d30c828c5573632fbd14d7168d868c29623e794ce31132`.

Raw public-safe final state: `soak-final.json`.

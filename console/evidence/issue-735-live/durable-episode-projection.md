# Durable episode projection evidence

Result: PASS.

The accepted project closeout produced one effective source event and one
source-linked building-memory episode. The episode receipt binds source event,
source row, projection version, request digest, episode ID, and durable
frontier. Restart and replay did not duplicate the source event or the episode.
The final preflight observed the episode producer, hierarchy, and canonical
projection at one stable cut with backlog 0.

- Episode receipt SHA-256:
  `726b78a1677e201839db1642716ecd44153125dafdaab55b722dc35bae11b232`.
- Source-event readback SHA-256:
  `60e4a89f5909ed5a53e571e0e151b6134e602e0e8d80d35611d33e3ce262d083`.

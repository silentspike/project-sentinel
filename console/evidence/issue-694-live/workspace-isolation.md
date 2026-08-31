# Workbench workspace isolation

Result: PASS.

The production workbench selected `bwrap-landlock` and bound every invocation
to its tenant, project, work item, agent, workspace, runtime instance, profile,
and policy. Inputs were read-only and digest-bound; workspace and artifact roots
were writable only inside the assigned scope. Parent traversal, symlink/hardlink
replacement, foreign workspace access, host paths, secrets, and outbound
network access were denied. No unisolated fallback was available.

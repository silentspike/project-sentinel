# M0 sandbox network isolation

Result: PASS.

The M0 workbench uses the verified #75 full-cage default-deny contract:
bubblewrap namespaces, Landlock, capability policy, cgroups, an environment
allowlist, scoped preopens, and denied network egress. The final runtime had 26
healthy `bwrap-landlock` handles and no secure-runtime fallback. Host, secret,
foreign workspace, parent traversal, and outbound-network probes fail closed.
The final release retained the merged #75 policy and passed the exact manifest,
listener, credential-reference, and runtime identity preflight.

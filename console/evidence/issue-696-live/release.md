# Release and rollback evidence

Result: PASS.

Release Management promoted one immutable generation-19 manifest and issued
the delivery from that exact release. The snapshot-backed rollback rehearsal
deployed the prior approved generation, proved the project/history remained
readable, and restored the accepted generation. Pre-rollback and post-restore
lineage files are byte-identical with SHA-256
`913ef1d94e18be67f033faa9e4598499d07f400630b0c75f7e3ca43f14e3ac43`.
The restored direct-lineage SHA-256 is
`c2cc49ec906826834f6cb669fbb484cf82d4f0ee243ea3b7e5cbf1644dfdad90`.

# Source-linked memory closeout

Result: PASS.

Closeout occurred only after explicit customer acceptance. The durable source
event is `project_closeout_published`; one effective event and operation were
observed. Its exact source row produced the `_building` episode with projection
version 1, request digest
`ebdf671487eafb47dfb9249c62fe78d1641553893201e66a6c271d488b812358`,
and a durable projection receipt. Background/Night-Run processing consumes this
provenance but cannot mutate workflow authority.

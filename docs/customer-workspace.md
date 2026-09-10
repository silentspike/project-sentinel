# Customer Workspace

The Console exposes the customer workspace at `/?view=customer`. The normal
Console remains the Operator workspace. Customer access does not start an
Operator WebTransport connection or use an Operator credential.

## Authentication

A customer signs in using the credential bound by the daemon's existing
workflow principal configuration to a customer, principal and tenant. An
Operator dashboard key is not a substitute. Serve this workspace over HTTPS.

The dashboard validates the credential at `/customer/workflow/identity` and
keeps it in a bounded in-memory session store. The browser receives only an
opaque `sentinel_customer_session` cookie: HttpOnly, SameSite=Strict, scoped to
`/api/customer`, with a one-hour lifetime and the configured Secure attribute.
Customer responses carry `Cache-Control: no-store`. A dashboard restart requires
customer sign-in again; it does not erase the workflow's durable operations.

Every customer request revalidates the complete identity at the daemon. A
revoked or changed identity invalidates the session. An upstream outage blocks
the request without treating it as a bad password or deleting the session.
The dedicated upstream client permits only the configured loopback daemon,
does not follow redirects or use environment proxies, and bounds time and body
size. Credentials are never returned to the browser or persisted in browser
storage.

## Commands And Reads

The browser can submit and clarify requests, inspect proposals, explicitly
accept or reject proposals, send feedback and inspect project work states.
The daemon's existing typed command validation remains authoritative for role,
ownership, state, version, budget and idempotency checks.

The overview reads only the authenticated customer's requests and agreements
inside the authenticated tenant. Project responses contain identifiers and work
states, not internal governance, agent authority or collaboration payloads.
Inbox reads are bounded to 128 requests and 128 projects and fail closed if the
limit is exceeded. No truncated list is presented as a complete inbox.

The routes are:

| Dashboard route | Purpose |
| --- | --- |
| `POST /api/customer/login` | Validate a customer credential and create a session |
| `POST /api/customer/logout` | Revoke that customer session |
| `GET /api/customer/status` | Revalidate and read the customer identity |
| `GET /api/customer/overview` | Read customer requests, proposals and project progress |
| `POST /api/customer/commands` | Forward an exact typed customer command |

## Interrupted Commands

Before dispatch, the browser persists the operation ID and exact command in
identity-scoped local storage. This contains customer request content, not
credentials. A Web Lock prevents simultaneous dispatch from multiple tabs.
Unavailable storage or Web Locks blocks dispatch rather than losing the replay
identity. Signing out does not delete an unresolved operation.

The dispatch marker is persisted before network I/O. Reloading retains the
same operation ID and payload. A successful response removes the local pending
record. A conclusive first-attempt client rejection allows input correction;
an error after any earlier uncertain dispatch does not clear the operation.
Retries of an uncertain operation always use the same ID and payload. There is
no automatic new-ID retry or automatic provider invocation.

## Acceptance Boundary

This surface does not by itself prove a delivered customer project. Preview
serving, explicit final delivery acceptance, the real agent-produced work and
the complete product journey require their own integration and live evidence.
Browser tests with intercepted API responses verify UI behavior only; they do
not replace daemon authorization tests or deployed product acceptance.

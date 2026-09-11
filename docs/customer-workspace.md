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
Project progress includes customer-owned delivery references and lifecycle states.
Each release reference is checked against its canonical stored digest; missing
or inconsistent release authority fails the read instead of displaying stale data.
The delivery list is bounded to 128 entries per project and does not expose
internal QA records, roles or credentials.
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
| `POST /api/customer/delivery` | Forward only a version-bound `confirm_delivery` intent |

The delivery proxy rejects project-only acceptance, internal QA/release intents,
missing references and caller-supplied identity or timestamps. It forwards the
original valid envelope with the revalidated server-side customer credential.
It never retries internally; a transport failure remains an unknown outcome.

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

## Sales Consultation

`send_customer_request_message` carries a request ID, expected request version,
message content and an optional `in_reply_to` message ID. The server derives the
author, role, timestamp and message ID from the authenticated operation. Sales
may ask one outstanding question with `in_reply_to: null`. Only the request's
customer may answer that exact question. A Sales credential cannot submit the
customer's answer, and another customer's credential cannot access the request.

The customer workspace displays this conversation and reserves replies through
the same durable browser command path. A failed response or reload reuses the
original operation, question ID, version and answer. The workflow commits each
message together with its event and idempotency result. Consultation history is
append-only; qualification is blocked until the customer answers. A duplicate
answer under a new operation is rejected, while an exact committed operation
can be replayed after restart without adding another message.

The older `clarify_customer_request` command is customer-only self-clarification;
Sales can no longer use it to write a question and purported customer answer
together. Previously persisted clarification records remain unchanged. Empty
consultation history is omitted when serializing old records, preserving their
existing digests. This command contract does not itself schedule a model call
or prove that an autonomous Sales agent has answered a live request.

## Acceptance Boundary

The authenticated daemon `POST /customer/workflow/preview` read accepts only
`project_id`, `delivery`, `release` and an optional `file` selector. Without a
selector it returns a bounded artifact inventory,
not HTML or filesystem paths. The customer must own the exact receipt, its
server-issued preview window must still be valid, and the release must remain
active. The release-manifest digest, project, tenant and preview digest must
agree. Expired, changed, rolled-back and foreign deliveries are rejected before
artifact storage access. Artifact owner principal identifiers are not exposed.
This inventory endpoint does not yet provide isolated browser rendering.

The customer-only BFF `POST /api/customer/preview` revalidates its separate
session and forwards the exact body with the server-held customer credential.
`file` contains only `artifact_id` and a manifest-declared relative `path`.
The daemon resolves the source agent and artifact kind from release/workflow
evidence, never from the browser. Reads use the pinned Workbench validator and
are limited to 1 MiB per file. The JSON response contains Base64-encoded bytes,
their length and the bound delivery/release/manifest references. Binary assets
are preserved without text conversion. Release revision and expiry are checked
again after file I/O; a concurrent change rejects the response. Responses are
not cached, and unavailable reads do not imply an uncertain business command.
This transport neither executes the HTML nor grants it customer-origin access.

### Document preview

The customer delivery row opens an artifact selector and a manifest-declared
HTML path. The browser checks the complete response binding and bounded UTF-8
payload before display. Changing the selection removes the old document; late
responses cannot replace another delivery. Overview refreshes preserve the open
preview, while expiry or a non-active delivery removes it.

A fixed broker document receives HTML only through a source- and channel-bound
message. It creates a second sandboxed frame for the artifact. The artifact has
no script permission, same-origin permission, forms, popups or top navigation.
Content Security Policy blocks external resources; the broker's frame policy
also blocks navigation of the child to remote URLs. Artifact HTML is never
inserted into the customer page or broker DOM. See the browser contracts for
[iframe sandboxing](https://developer.mozilla.org/en-US/docs/Web/HTML/Reference/Elements/iframe)
and [frame-src](https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Content-Security-Policy/frame-src).

This is a static document preview: inline CSS and embedded data images can be
rendered, but JavaScript, external styles/assets and multi-page resource routing
are not enabled. It does not replace functional website QA or prove the full
delivery journey. The adversarial browser fixture verifies inert scripts,
blocked image/frame/form/navigation requests, stable refresh, and revocation;
its intercepted responses do not constitute live backend acceptance.

The daemon delivery-intent protocol provides `confirm_delivery` for explicit
version-bound customer acceptance. Its intent contains `project_id`, `delivery`
and `release`; both references contain `id`, `generation` and `digest`. The
server includes these references in the durable operation digest and compares
them with its authoritative records before recording acceptance. A changed
reference is rejected, never silently replaced with a newer delivery. Customer
identity, timestamps, feedback and acceptance records remain server-derived.
The existing project-only `accept` intent remains compatible with the controlled
journey harness; the customer browser integration must use `confirm_delivery`.

The delivery row offers explicit acceptance only for an active, unexpired,
delivered receipt. A confirmation dialog identifies its delivery and version;
dismissing it causes no reservation or network mutation. Confirming persists
the exact project, delivery and release references before sending the customer
intent. The stored command determines its endpoint on every retry, including
after reload. A newer overview never replaces the reserved references. Pending
operations block new acceptance; accepted receipts cannot be accepted again
through the button. Server validation remains authoritative for stale views.

This surface does not by itself prove a delivered customer project. Preview
serving, explicit final delivery acceptance, the real agent-produced work and
the complete product journey require their own integration and live evidence.
Browser tests with intercepted API responses verify UI behavior only; they do
not replace daemon authorization tests or deployed product acceptance.

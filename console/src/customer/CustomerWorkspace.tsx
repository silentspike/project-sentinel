import { createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { customerFetch, CustomerApiError, dispatchReserved, pendingKey, readPending, reserveCommand, sendCustomerCommand,
  type CustomerIdentity, type Overview, type PendingCommand } from "./api";
import "./customer.css";
import { CustomerPreview } from "./CustomerPreview";

export function CustomerWorkspace() {
  const [identity, setIdentity] = createSignal<CustomerIdentity | null>(null);
  const [checking, setChecking] = createSignal(true);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  const [key, setKey] = createSignal("");
  const [data, setData] = createSignal<Overview>({ requests: [], proposals: [] });
  const [selected, setSelected] = createSignal("");
  const [summary, setSummary] = createSignal("");
  const [outcome, setOutcome] = createSignal("");
  const [constraints, setConstraints] = createSignal("");
  const [feedback, setFeedback] = createSignal("");
  const [question, setQuestion] = createSignal("");
  const [answer, setAnswer] = createSignal("");
  const [reply, setReply] = createSignal("");
  const [pending, setPending] = createSignal<PendingCommand | null>(null);
  const [previewId, setPreviewId] = createSignal("");
  const current = createMemo(() => data().requests.find(value => value.request_id === selected()));
  const preview = createMemo(() => {
    const project = data().projects?.find(value => value.request_id === selected() && value.deliveries?.some(delivery => delivery.delivery.id === previewId()));
    const delivery = project?.deliveries?.find(value => value.delivery.id === previewId());
    return project && delivery ? { projectId: project.project_id, delivery } : undefined;
  });
  const proposals = createMemo(() => data().proposals.filter(value => value.request_id === selected()));
  const disabled = () => busy() || pending() !== null;
  let refreshing = false;

  const report = (cause: unknown) => {
    if (cause instanceof CustomerApiError && cause.status === 401) {
      setIdentity(null); setData({ requests: [], proposals: [] });
      setError("Sitzung abgelaufen. Bitte erneut anmelden.");
    } else setError(cause instanceof Error ? cause.message : "Anfrage fehlgeschlagen.");
  };
  const refresh = async () => {
    const actor = identity();
    if (!actor || refreshing) return;
    refreshing = true;
    try {
      const value = await customerFetch<Overview>("overview");
      if (identity() !== actor) return;
      setData(value);
      if (!value.requests.some(request => request.request_id === selected())) setSelected(value.requests[0]?.request_id ?? "");
    } finally { refreshing = false; }
  };
  const establish = async (value: CustomerIdentity) => {
    setIdentity(value);
    setPending(readPending(localStorage, value));
    await refresh();
  };
  onMount(async () => {
    try {
      const value = await customerFetch<{ authenticated: boolean; identity?: CustomerIdentity }>("status");
      if (value.authenticated && value.identity) await establish(value.identity);
    } catch (cause) { report(cause); }
    finally { setChecking(false); }
  });
  const refreshTimer = window.setInterval(() => {
    if (identity() && !busy()) void refresh().catch(report);
  }, 2000);
  onCleanup(() => window.clearInterval(refreshTimer));
  const login = async (event: SubmitEvent) => {
    event.preventDefault(); setBusy(true); setError("");
    const credential = key(); setKey("");
    try {
      const value = await customerFetch<{ identity: CustomerIdentity }>("login", { key: credential });
      await establish(value.identity);
    } catch (cause) { report(cause); }
    finally { setBusy(false); }
  };
  const dispatch = async (value: PendingCommand) => {
    const actor = identity();
    if (!actor) return;
    setBusy(true); setError("");
    try {
      if (!navigator.locks) throw new Error("customer_command_lock_unavailable");
      await navigator.locks.request(pendingKey(actor), { ifAvailable: true }, async lock => {
        if (!lock) throw new Error("customer_command_active_in_another_tab");
        await dispatchReserved(localStorage, actor, value, sendCustomerCommand);
      });
      setPending(null);
      setSummary(""); setOutcome(""); setConstraints(""); setFeedback("");
      setQuestion(""); setAnswer(""); setReply("");
      await refresh();
    } catch (cause) {
      setPending(readPending(localStorage, actor));
      report(cause);
    } finally { setBusy(false); }
  };
  const command = async (value: Record<string, unknown>) => {
    const actor = identity();
    if (!actor || disabled()) return;
    try {
      const reserved = reserveCommand(localStorage, actor, value);
      setPending(reserved); await dispatch(reserved);
    } catch (cause) { report(cause); }
  };

  return <main class="customer-workspace">
    <header class="customer-header"><div><h1>Project Sentinel</h1><span class="muted">Kundenauftraege</span></div>
      <nav aria-label="Kundennavigation"><a href="/">Operator-Konsole</a>
        <Show when={identity()}>{actor => <><span>{actor().customer_id}</span><button disabled={busy()} onClick={async () => {
          setBusy(true);
          try { await customerFetch("logout", {}); setIdentity(null); setData({ requests: [], proposals: [] }); setPending(null); }
          catch (cause) { report(cause); } finally { setBusy(false); }
        }}>Abmelden</button></>}</Show>
      </nav>
    </header>
    <Show when={error()}><p class="customer-error" role="alert">{error()}</p></Show>
    <Show when={!checking()} fallback={<p role="status">Sitzung wird geprueft...</p>}>
      <Show when={identity()} fallback={<form class="customer-login" onSubmit={login}>
        <h2>Kundenanmeldung</h2><label>Kundenschluessel<input type="password" autocomplete="current-password" required minLength={32} maxLength={512} value={key()} onInput={event => setKey(event.currentTarget.value)} /></label>
        <button class="primary" disabled={busy()} type="submit">{busy() ? "Anmeldung..." : "Anmelden"}</button>
      </form>}>
        <Show when={pending()}>{value => <section class="customer-pending" aria-label="Ausstehende Bestaetigung">
          <strong>Bestaetigung ausstehend</strong><p>Operation {value().operation_id}</p>
          <button disabled={busy()} onClick={() => dispatch(value())}>Gleiche Anfrage erneut pruefen</button>
        </section>}</Show>
        <div class="customer-layout">
          <aside><div class="customer-section-heading"><h2>Anfragen</h2><button disabled={busy()} onClick={async () => {
            setBusy(true); setError(""); try { await refresh(); } catch (cause) { report(cause); } finally { setBusy(false); }
          }}>Aktualisieren</button></div>
            <For each={data().requests} fallback={<p class="muted">Noch keine Anfragen.</p>}>{request =>
              <button class="customer-request" aria-pressed={selected() === request.request_id} onClick={() => { setSelected(request.request_id); setReply(""); }}>
                <strong>{request.summary_ref}</strong><span>{request.state}</span>
              </button>
            }</For>
            <form class="customer-form" onSubmit={event => { event.preventDefault(); void command({ command: "submit_customer_request", summary_ref: summary().trim(), desired_outcome: outcome().trim(), constraints: constraints().split("\n").map(value => value.trim()).filter(Boolean) }); }}>
              <h2>Neue Anfrage</h2>
              <label>Projekttitel<input required maxLength={512} value={summary()} onInput={event => setSummary(event.currentTarget.value)} /></label>
              <label>Gewuenschtes Ergebnis<textarea required maxLength={4096} rows={4} value={outcome()} onInput={event => setOutcome(event.currentTarget.value)} /></label>
              <label>Rahmenbedingungen<textarea rows={3} maxLength={4096} value={constraints()} onInput={event => setConstraints(event.currentTarget.value)} /></label>
              <button class="primary" disabled={disabled() || !summary().trim() || !outcome().trim()}>Anfrage senden</button>
            </form>
          </aside>
          <section class="customer-detail" aria-label="Anfragedetails"><Show when={current()} fallback={<h2>Keine Anfrage ausgewaehlt</h2>}>{request => <>
            <div class="customer-section-heading"><h2>{request().summary_ref}</h2><span class="pill">{request().state}</span></div>
            <p>{request().desired_outcome}</p><ul><For each={request().constraints}>{value => <li>{value}</li>}</For></ul>
            <For each={(data().projects ?? []).filter(project => project.request_id === request().request_id)}>{project => <section class="customer-history">
              <h3>Projektfortschritt</h3><p>{project.state}</p>
              <table><thead><tr><th>Arbeitspaket</th><th>Status</th></tr></thead><tbody><For each={project.work_items}>{work => <tr><td>{work.work_item_id}</td><td>{work.state}</td></tr>}</For></tbody></table>
              <Show when={project.deliveries?.length}><h3>Lieferungen</h3>
                <table><thead><tr><th>Lieferung</th><th>Version</th><th>Status</th></tr></thead>
                  <tbody><For each={project.deliveries}>{delivery => <tr>
                    <td>{delivery.delivery.id}<button class="customer-preview-open" disabled={delivery.release_state !== "active" || !["delivered", "accepted"].includes(delivery.state) || delivery.expires_at_ms <= Date.now()} onClick={() => setPreviewId(delivery.delivery.id)}>Vorschau oeffnen</button>
                      <button class="customer-preview-open" disabled={disabled() || delivery.release_state !== "active" || delivery.state !== "delivered" || delivery.expires_at_ms <= Date.now()} onClick={() => {
                        if (delivery.expires_at_ms <= Date.now()) return;
                        if (window.confirm(`Lieferung ${delivery.delivery.id}, Version ${delivery.delivery.generation}, verbindlich abnehmen?`)) void command({ command: "confirm_delivery", project_id: project.project_id, delivery: { ...delivery.delivery }, release: { ...delivery.release } });
                      }}>Lieferung abnehmen</button>
                    </td><td>{delivery.delivery.generation}</td><td>{delivery.state}</td>
                  </tr>}</For></tbody>
                </table>
              </Show>
            </section>}</For>
            <Show when={preview()}>{target => <CustomerPreview projectId={target().projectId} delivery={target().delivery} close={() => setPreviewId("")} />}</Show>
            <For each={request().clarifications}>{value => <section class="customer-history"><strong>{value.question_ref}</strong><p>{value.answer_ref}</p></section>}</For>
            <For each={request().consultation ?? []}>{message => <section class="customer-history">
              <h3>{message.role === "sales" ? "Sales" : "Ihre Antwort"}</h3><p>{message.content}</p>
              <Show when={message.role === "sales" && ["submitted", "clarifying"].includes(request().state) && !(request().consultation ?? []).some(value => value.in_reply_to === message.message_id)}>
                <form class="customer-form" onSubmit={event => {
                  event.preventDefault(); void command({ command: "send_customer_request_message", request_id: request().request_id, expected_version: request().version, in_reply_to: message.message_id, content: reply().trim() });
                }}>
                  <label>Ihre Antwort<textarea required rows={3} maxLength={4096} value={reply()} onInput={event => setReply(event.currentTarget.value)} /></label>
                  <button disabled={disabled() || !reply().trim()}>Antwort senden</button>
                </form>
              </Show>
            </section>}</For>
            <For each={proposals()}>{proposal => <article class="customer-proposal">
              <h3>Angebot</h3>
              <p>{proposal.scope}</p>
              <For each={[["Lieferumfang", proposal.deliverables], ["Abnahmekriterien", proposal.acceptance_criteria], ["Ausgeschlossen", proposal.exclusions], ["Annahmen", proposal.assumptions]] as [string, string[]][]}>{([title, values]) =>
                <Show when={values.length}><h4>{title}</h4><ul><For each={values}>{value => <li>{value}</li>}</For></ul></Show>
              }</For>
              <p>Kostenobergrenze: {(proposal.cost_ceiling_micros / 1_000_000).toLocaleString("de-AT", { style: "currency", currency: "USD" })}</p>
              <p>Gueltig bis {new Date(proposal.expires_at_unix_ms).toLocaleString("de-AT")}</p>
              <div class="customer-actions"><button class="primary" disabled={disabled() || request().state !== "proposed" || proposal.expires_at_unix_ms <= Date.now()} onClick={() => {
                if (window.confirm("Dieses Angebot verbindlich annehmen?")) void command({ command: "accept_proposal", request_id: request().request_id, expected_version: request().version, proposal_id: proposal.proposal_id, proposal_digest: proposal.proposal_digest });
              }}>Angebot annehmen</button>
              <button disabled={disabled() || request().state !== "proposed" || !feedback().trim()} onClick={() => {
                if (window.confirm("Dieses Angebot ablehnen?")) void command({ command: "reject_proposal", request_id: request().request_id, expected_version: request().version, proposal_id: proposal.proposal_id, proposal_digest: proposal.proposal_digest, reason_ref: feedback().trim() });
              }}>Angebot ablehnen</button></div>
            </article>}</For>
            <section class="customer-history"><h3>Rueckmeldungen</h3><For each={request().feedback}>{value => <p>{value.feedback_ref}</p>}</For></section>
            <Show when={["submitted", "clarifying"].includes(request().state)}><form class="customer-form" onSubmit={event => {
              event.preventDefault(); void command({ command: "clarify_customer_request", request_id: request().request_id, expected_version: request().version, question_ref: question().trim(), answer_ref: answer().trim() });
            }}><h3>Anfrage praezisieren</h3><label>Frage<input required maxLength={512} value={question()} onInput={event => setQuestion(event.currentTarget.value)} /></label>
              <label>Antwort<textarea required rows={3} maxLength={4096} value={answer()} onInput={event => setAnswer(event.currentTarget.value)} /></label>
              <button disabled={disabled() || !question().trim() || !answer().trim()}>Praezisierung senden</button>
            </form></Show>
            <form class="customer-form" onSubmit={event => { event.preventDefault(); void command({ command: "record_customer_feedback", request_id: request().request_id, expected_version: request().version, feedback_ref: feedback().trim() }); }}>
              <label>Nachricht<textarea required maxLength={4096} rows={3} value={feedback()} onInput={event => setFeedback(event.currentTarget.value)} /></label>
              <button disabled={disabled() || !feedback().trim()}>Rueckmeldung senden</button>
            </form>
          </>}</Show></section>
        </div>
      </Show>
    </Show>
  </main>;
}

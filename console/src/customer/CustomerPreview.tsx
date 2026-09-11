import { createEffect, createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import { customerFetch, type CustomerDelivery } from "./api";
import { previewBroker, previewHtml, samePreviewBinding, type PreviewBinding, type PreviewFile, type PreviewInventory } from "./preview";

export function CustomerPreview(props: { projectId: string; delivery: CustomerDelivery; close: () => void }) {
  const [inventory, setInventory] = createSignal<PreviewInventory>();
  const [artifact, setArtifact] = createSignal("");
  const [path, setPath] = createSignal("index.html");
  const [loading, setLoading] = createSignal(false);
  const [error, setError] = createSignal("");
  const [document, setDocument] = createSignal<{ broker: string; html: string; channel: string }>();
  const [now, setNow] = createSignal(Date.now());
  const timer = window.setInterval(() => setNow(Date.now()), 1000);
  let generation = 0;
  let frame: HTMLIFrameElement | undefined;
  const binding = (): PreviewBinding => ({ project_id: props.projectId, delivery: props.delivery.delivery, release: props.delivery.release });
  const bindingKey = createMemo(() => JSON.stringify(binding()));
  const active = () => props.delivery.release_state === "active"
    && ["delivered", "accepted"].includes(props.delivery.state) && now() < props.delivery.expires_at_ms;
  const receive = (event: MessageEvent) => {
    const value = document();
    if (value && active() && event.source === frame?.contentWindow
      && event.data?.kind === "preview-ready" && event.data.channel === value.channel) {
      frame?.contentWindow?.postMessage({ kind: "render", channel: value.channel, html: value.html }, "*");
    }
  };
  window.addEventListener("message", receive);
  onCleanup(() => { generation++; window.clearInterval(timer); window.removeEventListener("message", receive); });
  createEffect(() => { if (!active()) setDocument(undefined); });
  createEffect(() => {
    const expected = JSON.parse(bindingKey()) as PreviewBinding;
    const serial = ++generation;
    setDocument(undefined); setInventory(undefined); setError(""); setLoading(true);
    void customerFetch<PreviewInventory>("preview", expected).then(value => {
      if (generation !== serial) return;
      if (!samePreviewBinding(value, expected) || !/^[a-f0-9]{64}$/.test(value.manifest_digest)
        || !Array.isArray(value.artifacts) || value.artifacts.length < 1 || value.artifacts.length > 64
        || value.artifacts.some(item => typeof item.artifact_id !== "string" || !item.artifact_id || !/^[a-f0-9]{64}$/.test(item.digest))) {
        throw new Error("preview_inventory_invalid");
      }
      setInventory(value); setArtifact(value.artifacts.find(item => item.artifact_id.includes("source_tree"))?.artifact_id ?? value.artifacts[0].artifact_id);
    }).catch(cause => { if (generation === serial) setError(cause instanceof Error ? cause.message : "Vorschau nicht verfuegbar"); })
      .finally(() => { if (generation === serial) setLoading(false); });
  });
  const open = async (event: SubmitEvent) => {
    event.preventDefault();
    const expected = inventory();
    if (!expected || !active() || loading()) return;
    const serial = generation, selectedArtifact = artifact(), selectedPath = path().trim();
    setDocument(undefined); setLoading(true); setError("");
    try {
      if (!/\.html?$/i.test(selectedPath)) throw new Error("Bitte eine HTML-Datei auswaehlen.");
      const value = await customerFetch<PreviewFile>("preview", { ...binding(), file: { artifact_id: selectedArtifact, path: selectedPath } });
      if (generation !== serial || !active()) return;
      const html = previewHtml(value, expected, selectedArtifact, selectedPath);
      const channel = crypto.randomUUID().replaceAll("-", "");
      setDocument({ broker: previewBroker(channel), html, channel });
    } catch (cause) { if (generation === serial) setError(cause instanceof Error ? cause.message : "Vorschau nicht verfuegbar"); }
    finally { if (generation === serial) setLoading(false); }
  };
  return <section class="customer-preview" aria-label="Lieferungsvorschau">
    <div class="customer-section-heading"><h3>Vorschau · Version {props.delivery.delivery.generation}</h3><button onClick={props.close}>Schliessen</button></div>
    <Show when={!active()}><p role="status">Diese Vorschau ist nicht mehr verfuegbar.</p></Show>
    <Show when={error()}><p role="alert" class="customer-error">{error()}</p></Show>
    <form class="customer-preview-controls" onSubmit={open}>
      <label>Artefakt<select value={artifact()} disabled={loading()} onChange={event => { setArtifact(event.currentTarget.value); setDocument(undefined); }}>
        <For each={inventory()?.artifacts}>{item => <option value={item.artifact_id}>{item.artifact_id}</option>}</For>
      </select></label>
      <label>HTML-Datei<input value={path()} maxLength={1024} required disabled={loading()} onInput={event => { setPath(event.currentTarget.value); setDocument(undefined); }} /></label>
      <button type="submit" disabled={loading() || !active() || !inventory()}>{loading() ? "Laedt..." : "Vorschau laden"}</button>
    </form>
    <Show when={document()} keyed>{value => <iframe ref={frame} class="customer-preview-frame" title="Isolierte Lieferungsvorschau" sandbox="allow-scripts" referrerpolicy="no-referrer" srcdoc={value.broker} />}</Show>
  </section>;
}

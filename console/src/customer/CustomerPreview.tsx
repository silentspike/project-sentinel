import { createEffect, createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import { customerFetch, customerPreviewAvailable, type CustomerDelivery } from "./api";
import { PREVIEW_MAX_STYLE_BYTES, PREVIEW_MAX_STYLESHEETS, previewBroker, previewHtml, previewStylesheetPath,
  samePreviewBinding, type PreviewBinding, type PreviewFile, type PreviewInventory } from "./preview";

interface PreviewDocument { broker: string; html: string; channel: string; inventory: PreviewInventory; artifact: string; path: string }

export function CustomerPreview(props: { projectId: string; delivery: CustomerDelivery; close: () => void }) {
  const [inventory, setInventory] = createSignal<PreviewInventory>();
  const [artifact, setArtifact] = createSignal("");
  const [path, setPath] = createSignal("index.html");
  const [loading, setLoading] = createSignal(false);
  const [error, setError] = createSignal("");
  const [document, setDocument] = createSignal<PreviewDocument>();
  const [now, setNow] = createSignal(Date.now());
  const timer = window.setInterval(() => setNow(Date.now()), 1000);
  let generation = 0;
  let renderGeneration = 0;
  let frame: HTMLIFrameElement | undefined;
  let stylesRequested = "";
  const clearDocument = () => { renderGeneration++; setDocument(undefined); };
  const binding = (): PreviewBinding => ({ project_id: props.projectId, delivery: props.delivery.delivery, release: props.delivery.release,
    ...(props.delivery.preview_access ? { preview_access: props.delivery.preview_access.access } : {}) });
  const bindingKey = createMemo(() => JSON.stringify(binding()));
  const active = () => customerPreviewAvailable(props.delivery, now());
  const receive = (event: MessageEvent) => {
    const value = document();
    if (!value || !active() || event.source !== frame?.contentWindow || event.data?.channel !== value.channel) return;
    if (event.data.kind === "preview-ready") {
      frame?.contentWindow?.postMessage({ kind: "render", channel: value.channel, html: value.html }, "*");
    } else if (event.data.kind === "preview-stylesheets" && stylesRequested !== value.channel) {
      stylesRequested = value.channel;
      void loadStyles(value, event.data.hrefs);
    } else if (event.data.kind === "preview-error") {
      setDocument(undefined); setError("Vorschau konnte nicht sicher geladen werden.");
    }
  };
  const loadStyles = async (value: PreviewDocument, hrefs: unknown) => {
    try {
      if (!Array.isArray(hrefs) || hrefs.length < 1 || hrefs.length > PREVIEW_MAX_STYLESHEETS) throw new Error("preview_stylesheets_invalid");
      const styles: string[] = [], cache = new Map<string, string>();
      let total = 0;
      for (const href of hrefs) {
        const path = previewStylesheetPath(value.path, href);
        if (document() !== value || !active()) return;
        let css = cache.get(path);
        if (css === undefined) {
          const response = await customerFetch<PreviewFile>("preview", { project_id: value.inventory.project_id,
            delivery: value.inventory.delivery, release: value.inventory.release,
            ...(value.inventory.preview_access ? { preview_access: value.inventory.preview_access } : {}), file: { artifact_id: value.artifact, path } });
          if (document() !== value || !active()) return;
          css = previewHtml(response, value.inventory, value.artifact, path);
          cache.set(path, css);
        }
        total += new TextEncoder().encode(css).length;
        if (total > PREVIEW_MAX_STYLE_BYTES) throw new Error("preview_stylesheets_too_large");
        styles.push(css);
      }
      if (document() === value && active()) frame?.contentWindow?.postMessage({ kind: "stylesheets", channel: value.channel, styles }, "*");
    } catch (cause) {
      if (document() === value) { setDocument(undefined); setError(cause instanceof Error ? cause.message : "Vorschau nicht verfuegbar"); }
    }
  };
  window.addEventListener("message", receive);
  onCleanup(() => { generation++; renderGeneration++; window.clearInterval(timer); window.removeEventListener("message", receive); });
  createEffect(() => { if (!active()) clearDocument(); });
  createEffect(() => {
    const expected = JSON.parse(bindingKey()) as PreviewBinding;
    const serial = ++generation;
    clearDocument(); setInventory(undefined); setError(""); setLoading(true);
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
    clearDocument();
    const serial = generation, renderSerial = renderGeneration, selectedArtifact = artifact(), selectedPath = path().trim();
    setLoading(true); setError("");
    try {
      if (!/\.html?$/i.test(selectedPath)) throw new Error("Bitte eine HTML-Datei auswaehlen.");
      const value = await customerFetch<PreviewFile>("preview", { ...binding(), file: { artifact_id: selectedArtifact, path: selectedPath } });
      if (generation !== serial || renderGeneration !== renderSerial || !active()) return;
      const html = previewHtml(value, expected, selectedArtifact, selectedPath);
      const channel = crypto.randomUUID().replaceAll("-", "");
      setDocument({ broker: previewBroker(channel), html, channel, inventory: expected, artifact: selectedArtifact, path: selectedPath });
    } catch (cause) { if (generation === serial && renderGeneration === renderSerial) setError(cause instanceof Error ? cause.message : "Vorschau nicht verfuegbar"); }
    finally { if (generation === serial) setLoading(false); }
  };
  return <section class="customer-preview" aria-label="Lieferungsvorschau">
    <div class="customer-section-heading"><h3>Vorschau - Version {props.delivery.delivery.generation}</h3><button onClick={props.close}>Schliessen</button></div>
    <Show when={!active()}><p role="status">Diese Vorschau ist nicht mehr verfuegbar.</p></Show>
    <Show when={error()}><p role="alert" class="customer-error">{error()}</p></Show>
    <form class="customer-preview-controls" onSubmit={open}>
      <label>Artefakt<select value={artifact()} disabled={loading()} onChange={event => { setArtifact(event.currentTarget.value); clearDocument(); }}>
        <For each={inventory()?.artifacts}>{item => <option value={item.artifact_id}>{item.artifact_id}</option>}</For>
      </select></label>
      <label>HTML-Datei<input value={path()} maxLength={1024} required disabled={loading()} onInput={event => { setPath(event.currentTarget.value); clearDocument(); }} /></label>
      <button type="submit" disabled={loading() || !active() || !inventory()}>{loading() ? "Laedt..." : "Vorschau laden"}</button>
    </form>
    <Show when={document()} keyed>{value => <iframe ref={frame} class="customer-preview-frame" title="Isolierte Lieferungsvorschau" sandbox="allow-scripts" referrerpolicy="no-referrer" srcdoc={value.broker} />}</Show>
  </section>;
}

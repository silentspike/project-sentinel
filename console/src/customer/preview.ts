import type { DeliveryReference } from "./api";

export interface PreviewBinding {
  project_id: string;
  delivery: DeliveryReference;
  release: DeliveryReference;
}
export interface PreviewInventory extends PreviewBinding {
  manifest_digest: string;
  artifacts: { artifact_id: string; digest: string; media_type: string }[];
}
export interface PreviewFile extends PreviewBinding {
  manifest_digest: string;
  artifact_id: string;
  path: string;
  encoding: string;
  size_bytes: number;
  content: string;
}

export function samePreviewBinding(value: PreviewBinding, expected: PreviewBinding): boolean {
  const same = (a: DeliveryReference, b: DeliveryReference) => a?.id === b.id && a?.generation === b.generation && a?.digest === b.digest;
  return value?.project_id === expected.project_id && same(value?.delivery, expected.delivery) && same(value?.release, expected.release);
}

export function previewHtml(value: PreviewFile, expected: PreviewInventory, artifact: string, path: string): string {
  if (!samePreviewBinding(value, expected) || value.manifest_digest !== expected.manifest_digest
    || value.artifact_id !== artifact || value.path !== path || value.encoding !== "base64"
    || !Number.isSafeInteger(value.size_bytes) || value.size_bytes < 1 || value.size_bytes > 1024 * 1024
    || typeof value.content !== "string" || value.content.length > 1_398_104
    || !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(value.content)) {
    throw new Error("preview_response_binding_invalid");
  }
  const bytes = Uint8Array.from(atob(value.content), character => character.charCodeAt(0));
  if (bytes.length !== value.size_bytes) throw new Error("preview_response_size_invalid");
  return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
}

// Only this fixed document runs in the broker frame. Artifact HTML is delivered
// by a source-checked message and never inserted into the broker's DOM.
export function previewBroker(channel: string): string {
  if (!/^[a-f0-9]{32}$/.test(channel)) throw new Error("preview_channel_invalid");
  return `<!doctype html><html><head><meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; font-src data:; frame-src blob:; connect-src 'none'; form-action 'none'; base-uri 'none'">
<style>html,body,iframe{margin:0;width:100%;height:100%;border:0;background:white}iframe{display:block}</style>
</head><body><script>
"use strict";
const channel=${JSON.stringify(channel)};
let used=false,url;
addEventListener("message",event=>{
 if(used||event.source!==parent||!event.data||event.data.channel!==channel||event.data.kind!=="render"||typeof event.data.html!=="string"||event.data.html.length>1048576)return;
 used=true;
 const policy="default-src 'none'; script-src 'none'; style-src 'unsafe-inline'; img-src data:; font-src data:; connect-src 'none'; frame-src 'none'; worker-src 'none'; form-action 'none'; base-uri 'none'";
 const prefix='<!doctype html><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="'+policy+'">';
 const frame=document.createElement("iframe");
 frame.title="Gelieferte Webseite";
 frame.setAttribute("sandbox","");
 frame.referrerPolicy="no-referrer";
 url=URL.createObjectURL(new Blob([prefix,event.data.html],{type:"text/html"}));
 frame.src=url;document.body.append(frame);
});
addEventListener("pagehide",()=>{if(url)URL.revokeObjectURL(url)});
parent.postMessage({kind:"preview-ready",channel},"*");
</script></body></html>`;
}

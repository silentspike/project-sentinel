import type { DeliveryReference } from "./api";

export interface PreviewBinding {
  project_id: string;
  delivery: DeliveryReference;
  release: DeliveryReference;
  preview_access?: DeliveryReference | null;
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
  return value?.project_id === expected.project_id && same(value?.delivery, expected.delivery) && same(value?.release, expected.release)
    && (expected.preview_access ? same(value?.preview_access as DeliveryReference, expected.preview_access) : !value?.preview_access);
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

export const PREVIEW_MAX_STYLESHEETS = 16;
export const PREVIEW_MAX_STYLE_BYTES = 1024 * 1024;

export function previewStylesheetPath(page: string, href: unknown): string {
  const invalid = () => new Error("preview_stylesheet_path_invalid");
  if (typeof href !== "string" || href.length < 1 || href.length > 1024) throw invalid();
  let decoded: string;
  try { decoded = decodeURIComponent(href); } catch { throw invalid(); }
  if (/[\\\x00-\x20\x7f:%?#]/.test(decoded) || decoded.startsWith("//")
    || /[\\\x00-\x20\x7f:%?#]/.test(page) || page.startsWith("/")
    || page.split("/").some(part => !part || part === "." || part === "..")) throw invalid();
  const parts = decoded.startsWith("/") ? [] : page.split("/").slice(0, -1);
  for (const part of decoded.split("/")) {
    if (part === "..") { if (!parts.length) throw invalid(); parts.pop(); }
    else if (part && part !== ".") parts.push(part);
  }
  const base = new URL(page, "https://preview.invalid/");
  const url = new URL(decoded, base);
  const path = decodeURIComponent(url.pathname).slice(1);
  if (url.origin !== base.origin || url.search || url.hash || path !== parts.join("/") || !/\.css$/i.test(path)) throw invalid();
  return path;
}

// Only this fixed document runs in the broker frame. Artifact HTML is delivered
// by a source-checked message and parsed only in this network-denied opaque frame.
export function previewBroker(channel: string): string {
  if (!/^[a-f0-9]{32}$/.test(channel)) throw new Error("preview_channel_invalid");
  return `<!doctype html><html><head><meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; font-src data:; frame-src blob:; connect-src 'none'; form-action 'none'; base-uri 'none'">
<style>html,body,iframe{margin:0;width:100%;height:100%;border:0;background:white}iframe{display:block}</style>
</head><body><script>
"use strict";
const channel=${JSON.stringify(channel)};
let parsed,links,url,state="ready";
const send=(kind,extra={})=>parent.postMessage({kind,channel,...extra},"*");
const fail=()=>{state="failed";send("preview-error")};
const render=()=>{
 const html="<!doctype html>"+parsed.documentElement.outerHTML;
 if(html.length>6291456){fail();return}
 state="rendered";
 const policy="default-src 'none'; script-src 'none'; style-src 'unsafe-inline'; img-src data:; font-src data:; connect-src 'none'; frame-src 'none'; worker-src 'none'; form-action 'none'; base-uri 'none'";
 const prefix='<!doctype html><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="'+policy+'">';
 const frame=document.createElement("iframe");
 frame.title="Gelieferte Webseite";
 frame.setAttribute("sandbox","");
 frame.referrerPolicy="no-referrer";
 url=URL.createObjectURL(new Blob([prefix,html],{type:"text/html"}));
 frame.src=url;document.body.append(frame);
 send("preview-rendered");
};
addEventListener("message",event=>{
 if(event.source!==parent||!event.data||event.data.channel!==channel)return;
 const data=event.data;
 if(state==="ready"&&data.kind==="render"&&typeof data.html==="string"&&data.html.length<=1048576){
  state="parsing";
  parsed=new DOMParser().parseFromString(data.html,"text/html");
  parsed.querySelectorAll("meta[http-equiv],base,script,iframe,object,embed,form").forEach(node=>node.remove());
  parsed.querySelectorAll("a,area").forEach(node=>{node.removeAttribute("href");node.removeAttributeNS("http://www.w3.org/1999/xlink","href");node.removeAttribute("target")});
  links=Array.from(parsed.querySelectorAll("link")).filter(node=>node.relList.contains("stylesheet"));
  if(links.length>${PREVIEW_MAX_STYLESHEETS}){fail();return}
  parsed.querySelectorAll("link").forEach(node=>{if(!links.includes(node))node.remove()});
  if(!links.length){render();return}
  state="styles";
  send("preview-stylesheets",{hrefs:links.map(node=>node.getAttribute("href"))});
 }else if(state==="styles"&&data.kind==="stylesheets"){
  if(!Array.isArray(data.styles)||data.styles.length!==links.length||data.styles.some(css=>typeof css!=="string")
   ||data.styles.reduce((size,css)=>size+new TextEncoder().encode(css).length,0)>${PREVIEW_MAX_STYLE_BYTES}){fail();return}
  links.forEach((node,index)=>{
   const style=parsed.createElement("style");
   if(node.hasAttribute("media"))style.setAttribute("media",node.getAttribute("media"));
   style.textContent=data.styles[index].replaceAll("<",String.fromCharCode(92)+"3c ");
   node.replaceWith(style);
  });
  render();
 }
});
addEventListener("pagehide",()=>{if(url)URL.revokeObjectURL(url)});
send("preview-ready");
</script></body></html>`;
}

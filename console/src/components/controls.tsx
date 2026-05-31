import { createSignal, For, Show, type JSX } from "solid-js";
import type { ConnectionStatus } from "../transport/client";

// Wiederverwendbare Controls (#419) — portiert nach noaide `components/togaf/controls/`.
// Funktionales Design; CSS-Vars-Theming.

export function ProgressBar(props: { done: number; total: number; label?: string }) {
  const pct = () => (props.total > 0 ? Math.round((props.done / props.total) * 100) : 0);
  return (
    <div data-testid="ctl-progressbar" style={{ margin: "6px 0" }}>
      <Show when={props.label}>
        <div class="muted" style={{ "font-size": "12px" }}>{props.label} — {pct()}%</div>
      </Show>
      <div style={{ background: "var(--surface-2)", "border-radius": "999px", height: "8px", overflow: "hidden" }}>
        <div style={{ width: `${pct()}%`, height: "100%", background: "var(--accent)" }} />
      </div>
    </div>
  );
}

export function SearchFilter(props: { placeholder?: string; onFilter: (q: string) => void }) {
  return (
    <input
      data-testid="ctl-searchfilter"
      type="search"
      placeholder={props.placeholder ?? "Filter…"}
      onInput={(e) => props.onFilter(e.currentTarget.value)}
      style={{ width: "100%" }}
    />
  );
}

export type Status = "pending" | "in_progress" | "done" | "blocked";
export function StatusDropdown(props: { value: Status; onChange?: (s: Status) => void }) {
  const opts: Status[] = ["pending", "in_progress", "done", "blocked"];
  return (
    <select
      data-testid="ctl-statusdropdown"
      value={props.value}
      onChange={(e) => props.onChange?.(e.currentTarget.value as Status)}
    >
      <For each={opts}>{(o) => <option value={o}>{o}</option>}</For>
    </select>
  );
}

export function LiveIndicator(props: { status: ConnectionStatus; label?: boolean }) {
  const color = () =>
    props.status === "connected" ? "var(--accent)" : props.status === "connecting" ? "var(--warn)" : "var(--danger)";
  return (
    <span data-testid="ctl-liveindicator" style={{ display: "inline-flex", "align-items": "center", gap: "6px" }}>
      <span style={{ width: "10px", height: "10px", "border-radius": "999px", background: color() }} />
      <Show when={props.label !== false}><span class="muted" style={{ "font-size": "12px" }}>{props.status}</span></Show>
    </span>
  );
}

export function ThemeToggle(props: { onToggle?: (dark: boolean) => void }) {
  const [dark, setDark] = createSignal(true);
  return (
    <button
      data-testid="ctl-themetoggle"
      onClick={() => { const d = !dark(); setDark(d); props.onToggle?.(d); }}
    >
      {dark() ? "🌙 Dark" : "☀ Light"}
    </button>
  );
}

// ── Toast (global) ──
interface ToastMsg { id: number; text: string; type: "info" | "ok" | "warn" | "error" }
const [toasts, setToasts] = createSignal<ToastMsg[]>([]);
let nextId = 1;
export function addToast(text: string, type: ToastMsg["type"] = "info", durationMs = 4000) {
  const id = nextId++;
  setToasts((p) => [...p, { id, text, type }]);
  setTimeout(() => setToasts((p) => p.filter((t) => t.id !== id)), durationMs);
}
export function ToastContainer(): JSX.Element {
  const bg = (t: ToastMsg["type"]) =>
    t === "ok" ? "var(--accent)" : t === "warn" ? "var(--warn)" : t === "error" ? "var(--danger)" : "var(--surface-2)";
  return (
    <div data-testid="ctl-toasts" style={{ position: "fixed", bottom: "16px", right: "16px", display: "grid", gap: "8px", "z-index": 1000 }}>
      <For each={toasts()}>
        {(t) => (
          <div style={{ background: bg(t.type), color: "#0b0d12", padding: "8px 14px", "border-radius": "6px", "font-weight": 600 }}>
            {t.text}
          </div>
        )}
      </For>
    </div>
  );
}

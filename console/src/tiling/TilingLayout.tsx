import { onMount, onCleanup, Show, type JSX } from "solid-js";
import { type TileNode, type SplitNode, resizeSplit, focusLeaf, type PanelKind } from "./engine";

// Render der Tiling-Engine (#444): rekursiv auf CSS Grid. Split = `grid-template-{columns|rows}`
// aus `fraction` + Gutter dazwischen. Gutter-Pointer-Drag aktualisiert die Fraktion (60fps).
// Smooth Re-Tiling via CSS-Transition (GPU) + WAAPI-Fade-in neuer Panels. ResizeObserver cacht
// das Container-Rect fuer praezise Gutter→Fraktion-Mathematik.

const GUTTER = 6;
const TRANSITION = "grid-template-columns 180ms ease, grid-template-rows 180ms ease";

function Gutter(props: { split: SplitNode; rect: () => DOMRect | null }) {
  let dragging = false;
  const onMove = (e: PointerEvent) => {
    if (!dragging) return;
    const r = props.rect();
    if (!r) return;
    const f = props.split.dir === "row" ? (e.clientX - r.left) / r.width : (e.clientY - r.top) / r.height;
    resizeSplit(props.split.id, f);
  };
  const stop = () => {
    dragging = false;
    document.removeEventListener("pointermove", onMove);
    document.removeEventListener("pointerup", stop);
  };
  return (
    <div
      data-testid={`gutter-${props.split.id}`}
      onPointerDown={(e) => {
        e.preventDefault();
        dragging = true;
        document.addEventListener("pointermove", onMove);
        document.addEventListener("pointerup", stop);
      }}
      style={{
        background: "var(--border)",
        cursor: props.split.dir === "row" ? "col-resize" : "row-resize",
        [props.split.dir === "row" ? "width" : "height"]: `${GUTTER}px`,
        "touch-action": "none",
      }}
    />
  );
}

export function Tiling(props: { node: TileNode; renderPanel: (p: PanelKind, leafId: string) => JSX.Element }): JSX.Element {
  return (
    <Show
      when={props.node.kind === "split" ? (props.node as SplitNode) : null}
      fallback={<LeafTile node={props.node as Extract<TileNode, { kind: "leaf" }>} renderPanel={props.renderPanel} />}
    >
      {(split) => <SplitTile split={split()} renderPanel={props.renderPanel} />}
    </Show>
  );
}

function SplitTile(props: { split: SplitNode; renderPanel: (p: PanelKind, leafId: string) => JSX.Element }): JSX.Element {
  let el: HTMLDivElement | undefined;
  let rect: DOMRect | null = null;
  const getRect = () => rect;
  onMount(() => {
    if (!el) return;
    rect = el.getBoundingClientRect();
    const ro = new ResizeObserver(() => { if (el) rect = el.getBoundingClientRect(); });
    ro.observe(el);
    onCleanup(() => ro.disconnect());
  });
  const template = () => {
    const a = `${props.split.fraction}fr`;
    const b = `${1 - props.split.fraction}fr`;
    return `${a} ${GUTTER}px ${b}`;
  };
  return (
    <div
      ref={el}
      data-testid={`split-${props.split.id}`}
      style={{
        display: "grid",
        [props.split.dir === "row" ? "grid-template-columns" : "grid-template-rows"]: template(),
        gap: 0,
        height: "100%",
        width: "100%",
        "min-height": 0,
        "min-width": 0,
        transition: TRANSITION,
      }}
    >
      <div style={{ "min-width": 0, "min-height": 0, overflow: "hidden" }}>
        <Tiling node={props.split.a} renderPanel={props.renderPanel} />
      </div>
      <Gutter split={props.split} rect={getRect} />
      <div style={{ "min-width": 0, "min-height": 0, overflow: "hidden" }}>
        <Tiling node={props.split.b} renderPanel={props.renderPanel} />
      </div>
    </div>
  );
}

function LeafTile(props: { node: Extract<TileNode, { kind: "leaf" }>; renderPanel: (p: PanelKind, leafId: string) => JSX.Element }): JSX.Element {
  let el: HTMLDivElement | undefined;
  onMount(() => {
    // WAAPI-Fade-in beim Erscheinen eines neuen Panels (GPU: opacity/transform), kein Jank.
    el?.animate(
      [{ opacity: 0, transform: "scale(0.98)" }, { opacity: 1, transform: "scale(1)" }],
      { duration: 160, easing: "ease-out" },
    );
  });
  return (
    <div
      ref={el}
      data-testid={`tile-${props.node.panel}`}
      onPointerDown={() => focusLeaf(props.node.id)}
      style={{ height: "100%", width: "100%", "min-height": 0, "min-width": 0 }}
    >
      {props.renderPanel(props.node.panel, props.node.id)}
    </div>
  );
}

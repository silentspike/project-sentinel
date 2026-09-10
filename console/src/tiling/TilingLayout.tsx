import { createMemo, onMount, onCleanup, Show, type JSX } from "solid-js";
import { type TileNode, type SplitNode, resizeSplit, focusLeaf, minimumTileSize, TILE_GUTTER, type PanelKind } from "./engine";

// Render der Tiling-Engine (#444): rekursiv auf CSS Grid. Split = `grid-template-{columns|rows}`
// aus `fraction` + Gutter dazwischen. Gutter-Pointer-Drag aktualisiert die Fraktion (60fps).
// Smooth Re-Tiling via CSS-Transition + WAAPI-Fade-in neuer Panels.
// Das aktuelle Container-Rect bleibt auch nach Workspace-Scroll korrekt.

const GUTTER = TILE_GUTTER;
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
    document.removeEventListener("pointercancel", stop);
  };
  onCleanup(stop);
  return (
    <div
      data-testid={`gutter-${props.split.id}`}
      role="separator"
      tabIndex={0}
      aria-label="Resize workspace panes"
      aria-orientation={props.split.dir === "row" ? "vertical" : "horizontal"}
      aria-valuemin={10}
      aria-valuemax={90}
      aria-valuenow={Math.round(props.split.fraction * 100)}
      onKeyDown={(e) => {
        const decrease = props.split.dir === "row" ? "ArrowLeft" : "ArrowUp";
        const increase = props.split.dir === "row" ? "ArrowRight" : "ArrowDown";
        let next: number;
        if (e.key === decrease) next = props.split.fraction - 0.05;
        else if (e.key === increase) next = props.split.fraction + 0.05;
        else if (e.key === "Home") next = 0.1;
        else if (e.key === "End") next = 0.9;
        else return;
        e.preventDefault();
        resizeSplit(props.split.id, next);
      }}
      onPointerDown={(e) => {
        e.preventDefault();
        dragging = true;
        document.addEventListener("pointermove", onMove);
        document.addEventListener("pointerup", stop);
        document.addEventListener("pointercancel", stop);
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
  const getRect = () => el?.getBoundingClientRect() ?? null;
  const minimumA = createMemo(() => minimumTileSize(props.split.a));
  const minimumB = createMemo(() => minimumTileSize(props.split.b));
  const template = () => {
    const axis = props.split.dir === "row" ? "width" : "height";
    const a = `minmax(${minimumA()[axis]}px, ${props.split.fraction}fr)`;
    const b = `minmax(${minimumB()[axis]}px, ${1 - props.split.fraction}fr)`;
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
        "min-height": `${props.split.dir === "col" ? minimumA().height + GUTTER + minimumB().height : Math.max(minimumA().height, minimumB().height)}px`,
        "min-width": `${props.split.dir === "row" ? minimumA().width + GUTTER + minimumB().width : Math.max(minimumA().width, minimumB().width)}px`,
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

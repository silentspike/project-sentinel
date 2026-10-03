import { createMemo, createSignal, For, onMount, onCleanup, type JSX } from "solid-js";
import { type TileNode, type SplitNode, resizeSplit, focusLeaf, minimumTileSize, TILE_GUTTER, type PanelKind } from "./engine";
import { layoutTiles, type TileRect } from "./geometry";

// Keyed leaves own views independently of the split tree, preserving edits during re-tiling.

const GUTTER = TILE_GUTTER;
const TRANSITION = "left 180ms ease, top 180ms ease, width 180ms ease, height 180ms ease";

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

export function Tiling(props: { node: TileNode; renderPanel: (p: PanelKind, leafId: string, initialAgentId?: number) => JSX.Element }): JSX.Element {
  let el: HTMLDivElement | undefined;
  const [size, setSize] = createSignal({ width: 0, height: 0 });
  const minimum = createMemo(() => minimumTileSize(props.node));
  const layout = createMemo(() => layoutTiles(props.node, size().width, size().height));
  const position = (rect: TileRect): JSX.CSSProperties => ({
    position: "absolute", left: `${rect.x}px`, top: `${rect.y}px`,
    width: `${rect.width}px`, height: `${rect.height}px`,
    "min-width": 0, "min-height": 0, overflow: "hidden", transition: TRANSITION,
  });
  const getRect = (rect: TileRect) => {
    const surface = el?.getBoundingClientRect();
    return surface ? new DOMRect(surface.left + rect.x, surface.top + rect.y,
      rect.width, rect.height) : null;
  };
  onMount(() => {
    const measure = () => {
      if (el) setSize({ width: el.clientWidth, height: el.clientHeight });
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(el!);
    onCleanup(() => observer.disconnect());
  });
  return (
    <div
      ref={el}
      data-testid="tiling-surface"
      style={{
        position: "relative", height: "100%", width: "100%",
        "min-width": `${minimum().width}px`, "min-height": `${minimum().height}px`,
      }}
    >
      <For each={[...layout().leaves.keys()]}>{(id) => {
        const initial = layout().leaves.get(id)!;
        const slot = () => layout().leaves.get(id) ?? initial;
        return <div style={position(slot())}>
          <LeafTile node={slot().node} renderPanel={props.renderPanel} />
        </div>;
      }}</For>
      <For each={[...layout().gutters.keys()]}>{(id) => {
        const initial = layout().gutters.get(id)!;
        const slot = () => layout().gutters.get(id) ?? initial;
        return <div style={position(slot())} data-testid={`split-${id}`}>
          <Gutter split={slot().node} rect={() => getRect(slot().parent)} />
        </div>;
      }}</For>
    </div>
  );
}

function LeafTile(props: { node: Extract<TileNode, { kind: "leaf" }>; renderPanel: (p: PanelKind, leafId: string, initialAgentId?: number) => JSX.Element }): JSX.Element {
  const { id, panel, initialAgentId } = props.node;
  const content = props.renderPanel(panel, id, initialAgentId);
  let el: HTMLDivElement | undefined;
  onMount(() => {
    // WAAPI-Fade-in beim Erscheinen eines neuen Panels (GPU: opacity/transform), kein Jank.
    el?.animate?.(
      [{ opacity: 0, transform: "scale(0.98)" }, { opacity: 1, transform: "scale(1)" }],
      { duration: 160, easing: "ease-out" },
    );
  });
  return (
    <div
      ref={el}
      data-testid={`tile-${panel}`}
      onPointerDown={() => focusLeaf(id)}
      style={{ height: "100%", width: "100%", "min-height": 0, "min-width": 0 }}
    >
      {content}
    </div>
  );
}

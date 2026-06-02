import { createSignal, For, type JSX } from "solid-js";

// VirtualScroller (#419) — 10k+-tauglich. Fixed-height Rows + Overscan; rendert nur das sichtbare
// Fenster (O(viewport) DOM-Knoten statt O(N)). Inspiriert von noaide VirtualScroller.

export function VirtualScroller<T>(props: {
  items: T[];
  rowHeight: number;
  height: number;
  overscan?: number;
  renderRow: (item: T, index: number) => JSX.Element;
}): JSX.Element {
  const [scrollTop, setScrollTop] = createSignal(0);
  const overscan = () => props.overscan ?? 6;
  const total = () => props.items.length;
  const first = () => Math.max(0, Math.floor(scrollTop() / props.rowHeight) - overscan());
  const visibleCount = () => Math.ceil(props.height / props.rowHeight) + overscan() * 2;
  const last = () => Math.min(total(), first() + visibleCount());
  const slice = () => props.items.slice(first(), last());

  return (
    <div
      data-testid="virtual-scroller"
      style={{ height: `${props.height}px`, overflow: "auto", position: "relative", border: "1px solid var(--border)", "border-radius": "6px" }}
      onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)}
    >
      <div style={{ height: `${total() * props.rowHeight}px`, position: "relative" }}>
        <div style={{ position: "absolute", top: `${first() * props.rowHeight}px`, left: 0, right: 0 }}>
          <For each={slice()}>
            {(item, i) => (
              <div style={{ height: `${props.rowHeight}px`, display: "flex", "align-items": "center", padding: "0 12px", "border-bottom": "1px solid var(--border)" }}>
                {props.renderRow(item, first() + i())}
              </div>
            )}
          </For>
        </div>
      </div>
    </div>
  );
}

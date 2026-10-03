import { createSignal, onCleanup, onMount, type JSX } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import { Tiling } from "../src/tiling/TilingLayout";
import { layoutTiles } from "../src/tiling/geometry";
import { closeLeaf, leaf, minimumTileSize, splitLeaf, tilingTree, TILE_GUTTER, type TileNode } from "../src/tiling/engine";

afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

describe("tiling geometry", () => {
  it("allocates the full extent without violating nested minimums", () => {
    const root: TileNode = { kind: "split", id: "outer", dir: "row", fraction: 0.1,
      a: { kind: "split", id: "stack", dir: "col", fraction: 0.9,
        a: leaf("agents"), b: leaf("metrics") }, b: leaf("control") };
    const minimum = minimumTileSize(root);
    const result = layoutTiles(root, 100, 100);
    expect(result.leaves.size).toBe(3);
    for (const slot of result.leaves.values()) {
      expect(slot.width).toBeGreaterThanOrEqual(320);
      expect(slot.height).toBeGreaterThanOrEqual(220);
      expect(slot.x + slot.width).toBeLessThanOrEqual(minimum.width);
      expect(slot.y + slot.height).toBeLessThanOrEqual(minimum.height);
    }
    const gutter = result.gutters.get("outer")!;
    expect(gutter.width).toBe(TILE_GUTTER);
    expect(gutter.parent.width).toBe(minimum.width);
  });

  it("uses fractional space and safe minimums for non-finite viewport values", () => {
    const root: TileNode = { kind: "split", id: "root", dir: "row", fraction: 0.25,
      a: leaf("agents"), b: leaf("metrics") };
    const layout = layoutTiles(root, 1606, 600);
    expect(layout.leaves.get(root.a.id)!.width).toBe(400);
    expect(layout.leaves.get(root.b.id)!.width).toBe(1200);
    const fallback = layoutTiles(root, Number.NaN, Infinity);
    expect(fallback.leaves.get(root.a.id)!.width).toBe(320);
    expect(fallback.leaves.get(root.b.id)!.height).toBe(220);
  });
});

describe("keyed panel lifetime", () => {
  it("preserves views with the production store and keyboard gutter resizing", () => {
    vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
    const first = (node: TileNode): string => node.kind === "leaf" ? node.id : first(node.a);
    const id = first(tilingTree.root);
    const view = render(() => <Tiling node={tilingTree.root}
      renderPanel={(_, key) => <input data-testid={`production-${key}`} />} />);
    const draft = view.getByTestId(`production-${id}`) as HTMLInputElement;
    fireEvent.input(draft, { target: { value: "keep this edit" } });
    splitLeaf(id, "row", "agent-editor", 36);
    const find = (node: TileNode): Extract<TileNode, { kind: "split" }> | null => {
      if (node.kind === "leaf") return null;
      if (node.a.kind === "leaf" && node.a.id === id) return node;
      return find(node.a) ?? find(node.b);
    };
    const split = find(tilingTree.root)!;
    expect(split.b.kind === "leaf" && split.b.initialAgentId).toBe(36);
    fireEvent.keyDown(view.getByTestId(`gutter-${split.id}`), { key: "ArrowRight" });
    expect(split.fraction).toBeCloseTo(0.55);
    closeLeaf(split.b.id);
    expect(view.getByTestId(`production-${id}`)).toBe(draft);
    expect(draft.value).toBe("keep this edit");
  });

  it("preserves unsaved state across splitting, resizing and sibling closure, then disposes only the closed panel", () => {
    const disconnect = vi.fn();
    vi.stubGlobal("ResizeObserver", class { observe() {} disconnect = disconnect; });
    const editor = leaf("agent-editor");
    const sibling = leaf("org-chart");
    const [root, setRoot] = createSignal<TileNode>(editor);
    const mounted: string[] = [];
    const disposed: string[] = [];
    function Panel(props: { id: string }): JSX.Element {
      const [value, setValue] = createSignal("");
      onMount(() => mounted.push(props.id));
      onCleanup(() => disposed.push(props.id));
      return <input data-testid={`draft-${props.id}`} value={value()}
        onInput={event => setValue(event.currentTarget.value)} />;
    }
    const view = render(() => <Tiling node={root()}
      renderPanel={(_, id) => <Panel id={id} />} />);
    const draft = view.getByTestId(`draft-${editor.id}`) as HTMLInputElement;
    fireEvent.input(draft, { target: { value: "unsaved employee edit" } });
    const split: TileNode = { kind: "split", id: "test-split", dir: "row", fraction: 0.5,
      a: editor, b: sibling };
    setRoot(split);
    setRoot({ ...split, fraction: 0.7 });
    setRoot(editor);
    expect(view.getByTestId(`draft-${editor.id}`)).toBe(draft);
    expect(draft.value).toBe("unsaved employee edit");
    expect(mounted.filter(id => id === editor.id)).toHaveLength(1);
    expect(disposed).toEqual([sibling.id]);
    setRoot(sibling);
    expect(disposed).toEqual([sibling.id, editor.id]);
    view.unmount();
    expect(disposed).toEqual([sibling.id, editor.id, sibling.id]);
    expect(disconnect).toHaveBeenCalledOnce();
  });
});

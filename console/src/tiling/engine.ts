import { createStore, produce } from "solid-js/store";

// Leichte Tiling-Engine (#444, niri/Hyprland-Stil) — Signal-getriebener Binaerbaum, kein Lib-Ballast.
// Leaf = ein Panel; Split = zwei Kinder (row|col) mit `fraction` (Anteil des ersten Kindes, 0..1).
// Operationen (split/resize/close/openPanel) sind reine Baum-Transformationen im SolidJS-Store
// → fine-grained reaktiv, kein Voll-Rerender.

export type PanelKind = "agents" | "floorplan" | "metrics" | "profiling" | "cockpit" | "activity" | "chaos" | "chat" | "control" | "timetravel" | "gaia-wizard" | "agent-editor" | "config-editor" | "synthesis" | "cost" | "org-chart" | "agent-deep";
export type SplitDir = "row" | "col";

export interface LeafNode {
  kind: "leaf";
  id: string;
  panel: PanelKind;
}
export interface SplitNode {
  kind: "split";
  id: string;
  dir: SplitDir;
  fraction: number;
  a: TileNode;
  b: TileNode;
}
export type TileNode = LeafNode | SplitNode;

let counter = 1;
function nextId(prefix: string): string {
  return `${prefix}-${counter++}`;
}

export function leaf(panel: PanelKind): LeafNode {
  return { kind: "leaf", id: nextId("leaf"), panel };
}

/** Default-Workspace: drei Saeulen fuer die ersten migrierten Push-Views. */
export function defaultWorkspace(): TileNode {
  return {
    kind: "split",
    id: nextId("split"),
    dir: "row",
    fraction: 0.34,
    a: leaf("agents"),
    b: { kind: "split", id: nextId("split"), dir: "row", fraction: 0.5, a: leaf("floorplan"), b: leaf("metrics") },
  };
}

const [tree, setTree] = createStore<{ root: TileNode; focused: string }>({
  root: defaultWorkspace(),
  focused: "",
});

export const tilingTree = tree;

/** Tiefensuche: liefert das erste Leaf (oder das fokussierte, falls gesetzt). */
function firstLeaf(node: TileNode): LeafNode {
  return node.kind === "leaf" ? node : firstLeaf(node.a);
}

function findLeaf(node: TileNode, id: string): LeafNode | null {
  if (node.kind === "leaf") return node.id === id ? node : null;
  return findLeaf(node.a, id) ?? findLeaf(node.b, id);
}

/** Ersetzt rekursiv den Knoten mit `id` durch `replacement` (oder entfernt ihn via `null`-Replacement-Collapse). */
function mapNode(node: TileNode, id: string, fn: (n: TileNode) => TileNode | null): TileNode | null {
  if (node.id === id) return fn(node);
  if (node.kind === "split") {
    const a = mapNode(node.a, id, fn);
    const b = mapNode(node.b, id, fn);
    if (a === null) return b; // Kind entfernt → Split kollabiert auf Geschwister
    if (b === null) return a;
    if (a !== node.a || b !== node.b) return { ...node, a, b };
  }
  return node;
}

/** Splittet ein Leaf: das bestehende Panel bleibt, ein neues Panel kommt daneben/darunter. */
export function splitLeaf(leafId: string, dir: SplitDir, panel: PanelKind) {
  setTree(
    produce((s) => {
      const root = mapNode(s.root, leafId, (n) =>
        n.kind === "leaf" ? { kind: "split", id: nextId("split"), dir, fraction: 0.5, a: n, b: leaf(panel) } : n,
      );
      if (root) s.root = root;
    }),
  );
}

/** Schliesst ein Leaf; der Eltern-Split kollabiert auf das Geschwister-Panel. */
export function closeLeaf(leafId: string) {
  setTree(
    produce((s) => {
      const root = mapNode(s.root, leafId, () => null);
      if (root) s.root = root;
    }),
  );
}

/** Verschiebt die Fraktion eines Splits (Gutter-Drag), geklemmt auf [0.1, 0.9]. */
export function resizeSplit(splitId: string, fraction: number) {
  const f = Math.min(0.9, Math.max(0.1, fraction));
  setTree(
    produce((s) => {
      mapNode(s.root, splitId, (n) => {
        if (n.kind === "split") n.fraction = f;
        return n;
      });
    }),
  );
}

/** Gaia-Command: oeffnet kontextuell ein Panel (z.B. "zeig Floorplan") — splittet das fokussierte/erste Leaf. */
export function openPanel(panel: PanelKind, dir: SplitDir = "row") {
  const target = (tree.focused && findLeaf(tree.root, tree.focused)) || firstLeaf(tree.root);
  splitLeaf(target.id, dir, panel);
}

export function focusLeaf(leafId: string) {
  setTree("focused", leafId);
}

/** Zaehlt die Leaves (fuer Tests/Benchmarks). */
export function countLeaves(node: TileNode = tree.root): number {
  return node.kind === "leaf" ? 1 : countLeaves(node.a) + countLeaves(node.b);
}

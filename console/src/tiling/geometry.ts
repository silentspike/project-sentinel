import { minimumTileSize, TILE_GUTTER, type LeafNode, type SplitNode, type TileNode } from "./engine";

export interface TileRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface LeafPlacement extends TileRect { node: LeafNode }
export interface GutterPlacement extends TileRect { node: SplitNode; parent: TileRect }

/** Keep layout separate from component ownership: moving a leaf must not recreate its view. */
export function layoutTiles(root: TileNode, width: number, height: number) {
  const minimum = minimumTileSize(root);
  const leaves = new Map<string, LeafPlacement>();
  const gutters = new Map<string, GutterPlacement>();
  function visit(node: TileNode, rect: TileRect): void {
    if (node.kind === "leaf") {
      leaves.set(node.id, { ...rect, node });
      return;
    }
    const a = minimumTileSize(node.a);
    const b = minimumTileSize(node.b);
    const row = node.dir === "row";
    const available = (row ? rect.width : rect.height) - TILE_GUTTER;
    const first = Math.min(available - (row ? b.width : b.height),
      Math.max(row ? a.width : a.height, available * node.fraction));
    const second = available - first;
    visit(node.a, row ? { ...rect, width: first } : { ...rect, height: first });
    gutters.set(node.id, { node, parent: rect,
      x: rect.x + (row ? first : 0), y: rect.y + (row ? 0 : first),
      width: row ? TILE_GUTTER : rect.width,
      height: row ? rect.height : TILE_GUTTER,
    });
    visit(node.b, row
      ? { ...rect, x: rect.x + first + TILE_GUTTER, width: second }
      : { ...rect, y: rect.y + first + TILE_GUTTER, height: second });
  }
  visit(root, { x: 0, y: 0,
    width: Math.max(minimum.width, Number.isFinite(width) ? width : 0),
    height: Math.max(minimum.height, Number.isFinite(height) ? height : 0),
  });
  return { leaves, gutters };
}

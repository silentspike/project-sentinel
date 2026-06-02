import { describe, it, expect } from "vitest";
import {
  defaultWorkspace, countLeaves, tilingTree, splitLeaf, closeLeaf, resizeSplit, openPanel,
  type TileNode,
} from "../src/tiling/engine";

function firstLeafId(n: TileNode): string {
  return n.kind === "leaf" ? n.id : firstLeafId(n.a);
}

describe("tiling engine (#444)", () => {
  it("default workspace has three pillars (3 leaves)", () => {
    expect(countLeaves(defaultWorkspace())).toBe(3);
    expect(countLeaves(tilingTree.root)).toBe(3);
  });

  it("splitLeaf adds a panel beside an existing leaf", () => {
    const before = countLeaves(tilingTree.root);
    splitLeaf(firstLeafId(tilingTree.root), "row", "floorplan");
    expect(countLeaves(tilingTree.root)).toBe(before + 1);
  });

  it("closeLeaf collapses the parent split onto the sibling", () => {
    const before = countLeaves(tilingTree.root);
    closeLeaf(firstLeafId(tilingTree.root));
    expect(countLeaves(tilingTree.root)).toBe(before - 1);
  });

  it("openPanel (Gaia command) inserts a panel", () => {
    const before = countLeaves(tilingTree.root);
    openPanel("timetravel");
    expect(countLeaves(tilingTree.root)).toBe(before + 1);
  });

  it("resizeSplit clamps the fraction to [0.1, 0.9]", () => {
    const root = tilingTree.root;
    if (root.kind === "split") {
      resizeSplit(root.id, 0.99);
      expect((tilingTree.root as Extract<TileNode, { kind: "split" }>).fraction).toBeLessThanOrEqual(0.9);
      resizeSplit(root.id, -1);
      expect((tilingTree.root as Extract<TileNode, { kind: "split" }>).fraction).toBeGreaterThanOrEqual(0.1);
    }
  });
});

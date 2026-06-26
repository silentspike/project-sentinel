import { createSignal } from "solid-js";

// #424: shared "which agent is selected for editing" — set by the Org Chart on a node click,
// consumed (and cleared) by the Agent Editor (#422). It decouples the two panels: `openPanel`
// (tiling/engine.ts) has no agent-id parameter, so this signal is the agent-id channel between
// "open the editor" and "for THIS agent".
export const [selectedAgentId, setSelectedAgentId] = createSignal<number | null>(null);

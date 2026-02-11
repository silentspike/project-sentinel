import { Hono } from "hono";
import { mockAgents, mockRooms, mockChat, mockMetrics } from "./data/mock";

const app = new Hono();
const startTime = Date.now();

// Health
app.get("/api/health", (c) =>
  c.json({ status: "ok", uptime: Math.floor((Date.now() - startTime) / 1000) })
);

// Agents
app.get("/api/agents", (c) =>
  c.json(mockAgents.map((a) => ({
    id: a.id,
    name: a.name,
    role: a.role,
    status: a.status,
    room: a.room,
    mood: a.mood,
  })))
);

// Agent state (by name, lowercase + hyphenated)
app.get("/api/agents/:name/state", (c) => {
  const name = c.req.param("name");
  const agent = mockAgents.find(
    (a) => a.name.toLowerCase().replace(/\s+/g, "-") === name
  );
  if (!agent) return c.json({ error: "Agent not found" }, 404);
  return c.json(agent);
});

// Rooms
app.get("/api/rooms", (c) => c.json(mockRooms));

// Room chat
app.get("/api/rooms/:id/chat", (c) => {
  const roomId = c.req.param("id");
  const messages = mockChat.filter((m) => m.room === roomId);
  return c.json(messages);
});

// Metrics
app.get("/api/metrics", (c) =>
  c.json({
    ...mockMetrics,
    uptime: Math.floor((Date.now() - startTime) / 1000),
  })
);

export default app;

// Server start only when executed directly
if (import.meta.main) {
  const port = 8000;
  console.log(`Starting dashboard server on http://localhost:${port}`);

  Bun.serve({
    port,
    fetch: app.fetch,
    websocket: {
      open(ws) {
        console.log("WebSocket client connected");
        const interval = setInterval(() => {
          const randomAgent = mockAgents[Math.floor(Math.random() * mockAgents.length)];
          ws.send(JSON.stringify({
            type: "bio_update",
            agent: randomAgent.name,
            data: {
              ...randomAgent.bio,
              hunger: Math.min(100, randomAgent.bio.hunger + Math.random() * 2),
            },
            tick: Date.now(),
          }));
        }, 5000);
        // @ts-expect-error Bun WebSocket extras
        ws.data = { interval };
      },
      message(_ws, _message) {
        // Client messages ignored (read-only dashboard)
      },
      close(ws) {
        // @ts-expect-error Bun WebSocket extras
        clearInterval(ws.data?.interval);
        console.log("WebSocket client disconnected");
      },
    },
  });

  console.log(`Dashboard running on http://localhost:${port}`);
}

import { Hono } from "hono";
import { serveStatic } from "hono/bun";
import { openDatabases } from "./db";
import { agentRoutes } from "./routes/agents";
import { roomRoutes } from "./routes/rooms";
import { metricRoutes } from "./routes/metrics";
import { cockpitRoutes } from "./routes/cockpit";
import { createWsHandler, startPolling } from "./ws";

const app = new Hono();

// API Routes
app.route("/api", agentRoutes);
app.route("/api", roomRoutes);
app.route("/api", metricRoutes);
app.route("/api", cockpitRoutes);

// Statische Dateien
app.use("/public/*", serveStatic({ root: "./" }));
app.get("/", serveStatic({ path: "./public/index.html" }));

// Named export fuer Tests (app.request() Pattern)
export { app };

// Server start bei direkter Ausfuehrung
if (import.meta.main) {
  const projDbPath =
    process.env.PROJECTION_DB_PATH || "data/projection.db";
  const esDbPath =
    process.env.EVENT_STORE_DB_PATH || "data/events.db";
  const port = parseInt(process.env.PORT || "3001", 10);

  try {
    openDatabases(projDbPath, esDbPath);
  } catch (err) {
    console.error(
      `Failed to open databases:\n  projection: ${projDbPath}\n  eventstore: ${esDbPath}\n`,
      err,
    );
    process.exit(1);
  }

  const wsHandler = createWsHandler();

  Bun.serve({
    port,
    fetch: app.fetch,
    websocket: wsHandler,
  });

  startPolling();
  console.log(`Dashboard running on http://localhost:${port}`);
}

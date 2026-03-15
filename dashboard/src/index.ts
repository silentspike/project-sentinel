import { Hono } from "hono";
import { serveStatic } from "hono/bun";
import { openDatabases } from "./db";
import { agentRoutes } from "./routes/agents";
import { roomRoutes } from "./routes/rooms";
import { metricRoutes } from "./routes/metrics";
import { cockpitRoutes } from "./routes/cockpit";
import { healthRoutes } from "./routes/health";
import { chaosRoutes } from "./routes/chaos";
import { chatRoutes } from "./routes/chat";
import { activityRoutes } from "./routes/activity";
import { controlRoutes } from "./routes/control";
import { eventRoutes } from "./routes/events";
import { createWsHandler, startPolling } from "./ws";

const app = new Hono();

// Root-level /health alias (for sentinel-health-monitor.timer compatibility)
app.route("/", healthRoutes);

// API Routes
app.route("/api", healthRoutes);
app.route("/api", agentRoutes);
app.route("/api", roomRoutes);
app.route("/api", metricRoutes);
app.route("/api", cockpitRoutes);
app.route("/api", chaosRoutes);
app.route("/api", chatRoutes);
app.route("/api", activityRoutes);
app.route("/api", controlRoutes);
app.route("/api", eventRoutes);

// Statische Dateien
app.use("/public/*", serveStatic({ root: "./" }));
app.get("/favicon.ico", serveStatic({ path: "./public/favicon.png" }));
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
    fetch(req, server) {
      const url = new URL(req.url);
      if (url.pathname === "/ws") {
        if (server.upgrade(req, { data: {} })) return undefined;
        return new Response("WebSocket upgrade failed", { status: 500 });
      }
      return app.fetch(req, server);
    },
    websocket: wsHandler,
    idleTimeout: 120,
  });

  startPolling();
  // Log connected WS clients periodically
  setInterval(() => {
    const { user, system } = process.cpuUsage();
    console.log(`[monitor] cpu_user=${(user/1e6).toFixed(1)}s cpu_sys=${(system/1e6).toFixed(1)}s`);
  }, 30000);
  console.log(`Dashboard running on http://localhost:${port}`);
}

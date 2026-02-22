import { Hono } from "hono";
import { getProjectionLag } from "../db";

export const healthRoutes = new Hono();

const startTime = Date.now();

healthRoutes.get("/health", (c) => {
  let projection_lag = 0;
  try {
    projection_lag = getProjectionLag();
  } catch {
    // DB not ready yet
  }
  return c.json({
    status: "ok",
    uptime: Math.floor((Date.now() - startTime) / 1000),
    projection_lag,
  });
});

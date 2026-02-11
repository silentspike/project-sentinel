import { describe, expect, it } from "bun:test";
import app from "../index";

describe("Dashboard API", () => {
  it("GET /api/health returns ok", async () => {
    const res = await app.request("/api/health");
    expect(res.status).toBe(200);
    const data = await res.json();
    expect(data.status).toBe("ok");
    expect(typeof data.uptime).toBe("number");
  });

  it("GET /api/agents returns agent list", async () => {
    const res = await app.request("/api/agents");
    expect(res.status).toBe(200);
    const data = await res.json();
    expect(Array.isArray(data)).toBe(true);
    expect(data.length).toBeGreaterThan(0);
    expect(data[0]).toHaveProperty("name");
    expect(data[0]).toHaveProperty("role");
  });

  it("GET /api/rooms returns 15 rooms", async () => {
    const res = await app.request("/api/rooms");
    expect(res.status).toBe(200);
    const data = await res.json();
    expect(Array.isArray(data)).toBe(true);
    expect(data.length).toBe(15);
  });

  it("GET /api/agents/:name/state returns full state", async () => {
    const res = await app.request("/api/agents/thomas-mueller/state");
    expect(res.status).toBe(200);
    const data = await res.json();
    expect(data.name).toBe("Thomas Mueller");
    expect(data.bio).toBeDefined();
    expect(typeof data.bio.hunger).toBe("number");
  });

  it("GET /api/rooms/:id/chat returns messages", async () => {
    const res = await app.request("/api/rooms/kueche/chat");
    expect(res.status).toBe(200);
    const data = await res.json();
    expect(Array.isArray(data)).toBe(true);
  });
});

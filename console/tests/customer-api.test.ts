import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { CustomerApiError, dispatchReserved, pendingKey, readPending, reserveCommand, sendCustomerCommand, type CustomerIdentity } from "../src/customer/api";

const identity: CustomerIdentity = { schema_version: 1, tenant_id: "tenant-a", customer_id: "customer-a", principal_id: "principal-a" };

describe("customer command reservations", () => {
  beforeEach(() => localStorage.clear());
  afterEach(() => vi.unstubAllGlobals());

  it("replays delivery confirmation on its original endpoint with exact references", async () => {
    const command = { command: "confirm_delivery", project_id: "project-one", delivery: { id: "delivery-one", generation: 2, digest: "a".repeat(64) }, release: { id: "release-one", generation: 3, digest: "b".repeat(64) } };
    const pending = reserveCommand(localStorage, identity, command);
    const fetcher = vi.fn().mockRejectedValueOnce(new Error("connection_lost")).mockResolvedValueOnce(new Response("{}"));
    vi.stubGlobal("fetch", fetcher);
    await expect(dispatchReserved(localStorage, identity, pending, sendCustomerCommand)).rejects.toThrow("connection_lost");
    await dispatchReserved(localStorage, identity, readPending(localStorage, identity)!, sendCustomerCommand);
    expect(fetcher).toHaveBeenCalledTimes(2);
    for (const [url, options] of fetcher.mock.calls) {
      expect(url).toBe("/api/customer/delivery");
      expect(JSON.parse(options.body)).toEqual({ operation_id: pending.operation_id, intent: { action: "confirm_delivery", project_id: command.project_id, delivery: command.delivery, release: command.release } });
    }
    expect(readPending(localStorage, identity)).toBeNull();
  });

  it("preserves the exact operation and payload across reload", () => {
    const command = { command: "submit_customer_request", summary_ref: "Studio", desired_outcome: "Website", constraints: [] };
    const reserved = reserveCommand(localStorage, identity, command);
    expect(readPending(localStorage, identity)).toEqual(reserved);
    expect(() => reserveCommand(localStorage, identity, command)).toThrow("customer_command_pending");
    expect(readPending(localStorage, identity)?.operation_id).toBe(reserved.operation_id);
  });

  it("does not reuse another tenant or customer's reservation", () => {
    reserveCommand(localStorage, identity, { command: "submit_customer_request" });
    expect(readPending(localStorage, { ...identity, tenant_id: "tenant-b" })).toBeNull();
    expect(readPending(localStorage, { ...identity, customer_id: "customer-b" })).toBeNull();
    expect(readPending(localStorage, { ...identity, principal_id: "principal-b" })).toBeNull();
  });

  it("fails closed on malformed persisted state", () => {
    localStorage.setItem(pendingKey(identity), '{"operation_id":"invalid","command":{}}');
    expect(() => readPending(localStorage, identity)).toThrow("customer_pending_record_invalid");
    expect(() => reserveCommand(localStorage, identity, { command: "submit_customer_request" })).toThrow();
  });

  it("cannot reserve when storage is unavailable", () => {
    const storage = { getItem: () => null, setItem: () => { throw new Error("quota"); } } as unknown as Storage;
    expect(() => reserveCommand(storage, identity, { command: "submit_customer_request" })).toThrow("quota");
  });

  it("releases a conclusively rejected first attempt", async () => {
    const pending = reserveCommand(localStorage, identity, { command: "accept_proposal" });
    await expect(dispatchReserved(localStorage, identity, pending, async () => { throw new CustomerApiError(409, "version_conflict"); })).rejects.toThrow("version_conflict");
    expect(readPending(localStorage, identity)).toBeNull();
  });

  it("does not mistake a later rejection for resolution of an unknown outcome", async () => {
    const pending = reserveCommand(localStorage, identity, { command: "accept_proposal" });
    await expect(dispatchReserved(localStorage, identity, pending, async () => { throw new Error("connection_lost"); })).rejects.toThrow();
    await expect(dispatchReserved(localStorage, identity, pending, async () => { throw new CustomerApiError(403, "authority_changed"); })).rejects.toThrow();
    expect(readPending(localStorage, identity)?.operation_id).toBe(pending.operation_id);
    await dispatchReserved(localStorage, identity, pending, async body => {
      expect(body).toEqual({ operation_id: pending.operation_id, command: pending.command });
    });
    expect(readPending(localStorage, identity)).toBeNull();
  });
});

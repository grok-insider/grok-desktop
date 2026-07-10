import { describe, expect, it, vi } from "vitest";
import { MockDesktopClient } from "./mockDesktopClient";

describe("MockDesktopClient", () => {
  it("keeps native artifact operations explicitly unavailable in browser preview", async () => {
    const client = new MockDesktopClient();

    await expect(client.importArtifact("atlas")).resolves.toEqual({
      status: "unavailable",
      reason: "File import is available only in the installed desktop application.",
    });
    await expect(client.openArtifact("asset-1", 8)).resolves.toEqual({
      status: "unavailable",
      reason: "Artifact opening is unavailable in the interface preview.",
    });
  });

  it("publishes an immutable snapshot after starting a run", async () => {
    const client = new MockDesktopClient();
    const listener = vi.fn();
    client.subscribe(listener);

    const { runId } = await client.startRun({
      prompt: "Inspect the release notes",
      mode: "work",
      projectId: "atlas",
      searchEnabled: true,
      researchEnabled: false,
    });

    const snapshot = await client.getSnapshot();
    expect(snapshot.runs[0]).toMatchObject({ id: runId, title: "Inspect the release notes", state: "planning" });
    expect(listener).toHaveBeenCalledOnce();

    snapshot.runs[0].title = "mutated by caller";
    expect((await client.getSnapshot()).runs[0].title).toBe("Inspect the release notes");
  });

  it("does not expose generic run-state or approval mutations", () => {
    const client = new MockDesktopClient();
    expect(client).not.toHaveProperty("setRunState");
    expect(client).not.toHaveProperty("resolveApproval");
  });

  it("publishes media queue transitions without renderer polling", async () => {
    vi.useFakeTimers();
    try {
      const client = new MockDesktopClient();
      const listener = vi.fn();
      client.subscribeMediaCreations("image", listener);

      await client.createMedia({ kind: "image", prompt: "Launch room", aspectRatio: "16:9" });
      expect(listener.mock.calls.at(-1)?.[0][0]).toMatchObject({ prompt: "Launch room", status: "queued" });
      await vi.advanceTimersByTimeAsync(80);
      expect(listener.mock.calls.at(-1)?.[0][0]).toMatchObject({ status: "generating", progress: 48 });
      await vi.advanceTimersByTimeAsync(140);
      expect(listener.mock.calls.at(-1)?.[0][0]).toMatchObject({ status: "completed", progress: 100 });
    } finally {
      vi.useRealTimers();
    }
  });

  it("creates projects through the typed client boundary", async () => {
    const client = new MockDesktopClient();
    const result = await client.createProject({ name: "Launch QA", description: "Release validation" });
    expect(result).toMatchObject({ status: "success", value: { name: "Launch QA" } });
    expect((await client.getSnapshot()).projects[0]).toMatchObject({ name: "Launch QA", description: "Release validation" });
  });

  it("models native credential enrollment without accepting a renderer secret", async () => {
    const client = new MockDesktopClient({ firstRun: true });
    expect((await client.getAccountSetup()).xaiApiKey).toBe("not_configured");
    await expect(client.enrollXaiApiKey()).resolves.toMatchObject({
      status: "success",
      value: { xaiApiKey: "configured" },
    });
  });

  it("never retries an uncertain provider-started cancellation", async () => {
    const client = new MockDesktopClient();
    const { threadId } = await client.startRun({
      prompt: "Perform this request once.",
      mode: "chat",
      projectId: "atlas",
      searchEnabled: false,
      researchEnabled: false,
    });
    const loaded = await client.getConversation(threadId);
    expect(loaded.status).toBe("success");
    if (loaded.status !== "success") return;
    const source = loaded.value.turns[0];
    const cancelled = await client.cancelConversationTurn({
      turnId: source.id,
      expectedRevision: source.revision,
    });
    expect(cancelled).toMatchObject({
      status: "success",
      value: {
        state: "interrupted_needs_review",
        retryEligibility: "source_interrupted_needs_review",
      },
    });

    await expect(client.retryConversationTurn({
      sourceTurnId: source.id,
      expectedRevision: source.revision + 1,
    })).resolves.toMatchObject({ status: "unavailable" });
  });
});

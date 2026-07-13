import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router-dom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { DesktopClientProvider } from "../services/DesktopClientContext";
import type { DesktopBridge, DesktopNavigationRoute } from "../contracts/bridge";
import type { WorkspaceSearchHit, WorkspaceSearchResults } from "../services/desktopClient";
import { MockDesktopClient } from "../services/mockDesktopClient";
import { App } from "./App";

function renderApp(initialEntry = "/") {
  const client = new MockDesktopClient();
  render(
    <DesktopClientProvider client={client}>
      <MemoryRouter initialEntries={[initialEntry]}>
        <App />
      </MemoryRouter>
    </DesktopClientProvider>,
  );
  return client;
}

class LimitedDesktopClient extends MockDesktopClient {
  override async getSnapshot() {
    const snapshot = await super.getSnapshot();
    snapshot.capabilities = snapshot.capabilities.map((capability) => {
      if (capability.id === "work") return { ...capability, available: false, availability: "unavailable" as const, reason: "Signed guest channel is not connected." };
      if (capability.id === "browser_automation") return { ...capability, available: false, availability: "unavailable" as const, reason: "Managed browser broker is not ready." };
      return capability;
    });
    return snapshot;
  }
}

class FailingPreferencesClient extends MockDesktopClient {
  override async updateDesktopPreferences(_input: {
    expectedRevision: number;
    keepRunningInNotificationArea: boolean;
  }): Promise<never> {
    throw new Error("daemon unavailable");
  }
}

class InterruptedRunClient extends MockDesktopClient {
  override async getSnapshot() {
    const snapshot = await super.getSnapshot();
    snapshot.runs = snapshot.runs.slice(0, 1).map((run) => ({
      ...run,
      state: "interrupted_needs_review" as const,
    }));
    return snapshot;
  }
}

class CanonicalSearchClient extends MockDesktopClient {
  readonly search = vi.fn(async (_input: { query: string }): Promise<WorkspaceSearchResults> => ({
    hits: [{
      id: "message-search-1",
      projectId: "atlas",
      threadId: "thread-1",
      kind: "message",
      title: "Q3 launch narrative",
      snippet: "Customer evidence and launch actions",
      updatedAtUnixMs: 10,
    }],
    hasMore: false,
  }));

  override searchWorkspace(input: { query: string }): Promise<WorkspaceSearchResults> {
    return this.search(input);
  }
}

class RecoveringSearchClient extends CanonicalSearchClient {
  private attempt = 0;

  override async searchWorkspace(input: { query: string }): Promise<WorkspaceSearchResults> {
    this.attempt += 1;
    if (this.attempt === 1) throw new Error("daemon unavailable");
    return super.searchWorkspace(input);
  }
}

class DeferredSearchClient extends MockDesktopClient {
  readonly requests: Array<{
    query: string;
    resolve(results: WorkspaceSearchResults): void;
  }> = [];

  override searchWorkspace(input: { query: string }): Promise<WorkspaceSearchResults> {
    return new Promise((resolve) => this.requests.push({ query: input.query, resolve }));
  }
}

class SearchHitClient extends MockDesktopClient {
  constructor(private readonly hit: WorkspaceSearchHit) {
    super();
  }

  override async searchWorkspace(): Promise<WorkspaceSearchResults> {
    return { hits: [this.hit], hasMore: false };
  }
}

function installNavigationBridge(): (route: DesktopNavigationRoute) => void {
  let listener: ((route: DesktopNavigationRoute) => void) | undefined;
  const bridge: DesktopBridge = {
    request: vi.fn(),
    onDaemonStatus: vi.fn(() => () => undefined),
    onConversationTurnEvents: vi.fn(() => () => undefined),
    onNavigationRoute: vi.fn((nextListener) => {
      listener = nextListener;
      return () => {
        if (listener === nextListener) listener = undefined;
      };
    }),
  };
  window.grokDesktop = bridge;
  return (route) => listener?.(route);
}

afterEach(() => {
  delete window.grokDesktop;
});

describe("Grok Desktop shell", () => {
  it("renders the primary workspace and loaded conversations", async () => {
    renderApp();
    expect(screen.getByRole("heading", { name: "What are we working on?" })).toBeInTheDocument();
    expect(await screen.findByText("Q3 launch narrative")).toBeInTheDocument();
    expect(screen.getByRole("navigation", { name: "Primary navigation" })).toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: /^(Pause|Resume) unavailable$/ })).not.toHaveLength(0);
    for (const control of screen.getAllByRole("button", { name: /^(Pause|Resume) unavailable$/ })) {
      expect(control).toBeDisabled();
    }
  });

  it("navigates typed deep-link routes delivered by the isolated preload bridge", async () => {
    const emitNavigation = installNavigationBridge();
    renderApp();
    await screen.findByRole("heading", { name: "What are we working on?" });

    act(() => emitNavigation({ version: 1, route: "conversation", threadId: "thread-1" }));
    expect(await screen.findByRole("heading", { name: "Q3 launch narrative" })).toBeInTheDocument();

    act(() => emitNavigation({ version: 1, route: "settings" }));
    expect(await screen.findByRole("heading", { name: "Settings" })).toBeInTheDocument();
  });

  it("labels and exercises the standalone interface preview", async () => {
    renderApp();
    expect(await screen.findByText("Sample data only. No Grok provider request or local tool execution is available.")).toBeInTheDocument();
    expect(screen.getByText("Interface preview only")).toBeInTheDocument();
    expect(screen.queryByText("Grok execution available")).not.toBeInTheDocument();
    const work = screen.getByRole("tab", { name: "Work" });
    await waitFor(() => expect(work).toBeEnabled());
    const user = userEvent.setup();
    await user.click(work);
    fireEvent.change(screen.getByLabelText("Message Grok"), { target: { value: "Prepare a launch readiness report" } });
    fireEvent.click(screen.getByRole("button", { name: "Send message" }));

    await waitFor(() => expect(screen.getByRole("heading", { name: "Prepare a launch readiness report" })).toBeInTheDocument());
    expect(screen.queryByRole("heading", { name: "Activity" })).not.toBeInTheDocument();
  });

  it("exposes project sections as keyboard-accessible tabs", async () => {
    renderApp("/projects/atlas");
    expect(await screen.findByRole("heading", { name: "Atlas launch" })).toBeInTheDocument();
    const user = userEvent.setup();
    await user.click(screen.getByRole("tab", { name: "Instructions" }));
    expect(screen.getByLabelText("Project instructions")).toHaveValue(
      "Prioritize evidence-backed recommendations and flag assumptions.",
    );
  });

  it("does not advertise unfinished Work or browser settings surfaces", async () => {
    const client = new LimitedDesktopClient();
    render(<DesktopClientProvider client={client}><MemoryRouter initialEntries={["/settings"]}><App /></MemoryRouter></DesktopClientProvider>);

    expect(await screen.findByRole("heading", { name: "Settings" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Execution" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Browser" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Open setup" })).toBeInTheDocument();
    expect(screen.queryByText("Protected Work is available")).not.toBeInTheDocument();
    expect(screen.queryByText("Managed browser broker is ready.")).not.toBeInTheDocument();
  });

  it("counts interrupted runs as needing input in global navigation", async () => {
    const client = new InterruptedRunClient();
    render(<DesktopClientProvider client={client}><MemoryRouter><App /></MemoryRouter></DesktopClientProvider>);

    expect(await screen.findByRole("link", { name: "Activity, 1 items need input" })).toBeInTheDocument();
  });

  it.each(["shortcut", "trigger"] as const)(
    "restores command-palette focus to its trigger after a %s open",
    async (openMethod) => {
      renderApp();
      const trigger = screen.getByRole("button", { name: /Search conversations, projects, and files/ });

      if (openMethod === "shortcut") {
        fireEvent.keyDown(window, { key: "k", ctrlKey: true });
      } else {
        fireEvent.click(trigger);
      }

      const input = screen.getByRole("combobox", { name: "Search everything" });
      await waitFor(() => expect(input).toHaveFocus());
      fireEvent.keyDown(document, { key: "Escape" });

      await waitFor(() => expect(screen.queryByRole("dialog", { name: "Search Grok Desktop" })).not.toBeInTheDocument());
      await waitFor(() => expect(trigger).toHaveFocus());
    },
  );

  it("uses daemon-owned search results and follows their canonical conversation route", async () => {
    const client = new CanonicalSearchClient();
    render(<DesktopClientProvider client={client}><MemoryRouter><App /></MemoryRouter></DesktopClientProvider>);

    fireEvent.keyDown(window, { key: "k", ctrlKey: true });
    const input = screen.getByRole("combobox", { name: "Search everything" });
    fireEvent.change(input, { target: { value: "customer evidence" } });

    expect(await screen.findByText("Customer evidence and launch actions")).toBeInTheDocument();
    expect(client.search).toHaveBeenCalledWith({ query: "customer evidence", limit: 8 });
    fireEvent.click(screen.getByRole("option", { name: /Q3 launch narrative/ }));
    expect(await screen.findByRole("heading", { name: "Q3 launch narrative" })).toBeInTheDocument();
  });

  it("announces canonical search failures and retries without closing the dialog", async () => {
    const client = new RecoveringSearchClient();
    render(<DesktopClientProvider client={client}><MemoryRouter><App /></MemoryRouter></DesktopClientProvider>);

    fireEvent.keyDown(window, { key: "k", ctrlKey: true });
    fireEvent.change(screen.getByRole("combobox", { name: "Search everything" }), {
      target: { value: "customer evidence" },
    });

    expect(await screen.findByRole("alert")).toHaveTextContent("Workspace search is unavailable");
    fireEvent.click(screen.getByRole("button", { name: "Retry search" }));
    expect(await screen.findByText("Customer evidence and launch actions")).toBeInTheDocument();
    expect(screen.getByRole("dialog", { name: "Search Grok Desktop" })).toBeInTheDocument();
  });

  it("never renders a stale result when an older search resolves after a newer query", async () => {
    const client = new DeferredSearchClient();
    render(<DesktopClientProvider client={client}><MemoryRouter><App /></MemoryRouter></DesktopClientProvider>);

    fireEvent.keyDown(window, { key: "k", ctrlKey: true });
    const input = screen.getByRole("combobox", { name: "Search everything" });
    fireEvent.change(input, { target: { value: "first query" } });
    await waitFor(() => expect(client.requests).toHaveLength(1));
    fireEvent.change(input, { target: { value: "second query" } });
    await waitFor(() => expect(client.requests).toHaveLength(2));

    await act(async () => {
      client.requests[0].resolve({
        hits: [{
          id: "message-stale",
          projectId: "atlas",
          threadId: "thread-1",
          kind: "message",
          title: "Stale result",
          snippet: "Must never be actionable",
          updatedAtUnixMs: 1,
        }],
        hasMore: false,
      });
      await Promise.resolve();
    });
    expect(screen.queryByText("Stale result")).not.toBeInTheDocument();

    await act(async () => {
      client.requests[1].resolve({
        hits: [{
          id: "message-current",
          projectId: "atlas",
          threadId: "thread-1",
          kind: "message",
          title: "Current result",
          snippet: "Safe to select",
          updatedAtUnixMs: 2,
        }],
        hasMore: false,
      });
      await Promise.resolve();
    });
    expect(await screen.findByText("Current result")).toBeInTheDocument();
    expect(screen.queryByText("Stale result")).not.toBeInTheDocument();
  });

  it("enforces the daemon's UTF-8 search byte limit before making a request", async () => {
    const client = new CanonicalSearchClient();
    render(<DesktopClientProvider client={client}><MemoryRouter><App /></MemoryRouter></DesktopClientProvider>);

    fireEvent.keyDown(window, { key: "k", ctrlKey: true });
    const input = screen.getByRole("combobox", { name: "Search everything" });
    fireEvent.change(input, { target: { value: "é".repeat(129) } });
    expect(await screen.findByRole("alert")).toHaveTextContent("limited to 256 UTF-8 bytes");
    expect(input).toHaveAttribute("aria-invalid", "true");
    expect(client.search).not.toHaveBeenCalled();

    fireEvent.change(input, { target: { value: "e\u0301".repeat(86) } });
    expect(await screen.findByRole("alert")).toHaveTextContent("limited to 256 UTF-8 bytes");
    expect(client.search).not.toHaveBeenCalled();

    fireEvent.change(input, { target: { value: "é".repeat(128) } });
    await waitFor(() => expect(client.search).toHaveBeenCalledWith({ query: "é".repeat(128), limit: 8 }));
    expect(input).not.toHaveAttribute("aria-invalid");
  });

  it.each([
    {
      hit: { id: "atlas", projectId: "atlas", kind: "project", title: "Atlas launch", snippet: "Project result", updatedAtUnixMs: 1 } as WorkspaceSearchHit,
      heading: "Atlas launch",
    },
    {
      hit: { id: "asset-1", projectId: "atlas", kind: "artifact", title: "Launch narrative.md", snippet: "Library result", updatedAtUnixMs: 1 } as WorkspaceSearchHit,
      heading: "Library",
    },
    {
      hit: { id: "automation-1", projectId: "atlas", kind: "automation", title: "Weekly launch brief", snippet: "Automation result", updatedAtUnixMs: 1 } as WorkspaceSearchHit,
      heading: "Automation definitions",
    },
  ])("routes canonical $hit.kind search hits", async ({ hit, heading }) => {
    const client = new SearchHitClient(hit);
    render(<DesktopClientProvider client={client}><MemoryRouter><App /></MemoryRouter></DesktopClientProvider>);
    fireEvent.keyDown(window, { key: "k", ctrlKey: true });
    fireEvent.change(screen.getByRole("combobox", { name: "Search everything" }), { target: { value: "result" } });
    fireEvent.click(await screen.findByRole("option", { name: new RegExp(hit.title, "i") }));
    expect(await screen.findByRole("heading", { name: heading })).toBeInTheDocument();
  });

  it("updates daemon-owned close behavior from General settings", async () => {
    const client = new MockDesktopClient();
    const update = vi.spyOn(client, "updateDesktopPreferences");
    render(<DesktopClientProvider client={client}><MemoryRouter initialEntries={["/settings"]}><App /></MemoryRouter></DesktopClientProvider>);

    fireEvent.click(await screen.findByRole("button", { name: "General" }));
    const toggle = await screen.findByRole("switch", { name: "Keep running in notification area" });
    expect(toggle).toBeChecked();
    fireEvent.click(toggle);

    await waitFor(() => expect(update).toHaveBeenCalledWith({
      expectedRevision: 0,
      keepRunningInNotificationArea: false,
      updateChannel: "stable",
    }));
    await waitFor(() => expect(toggle).not.toBeChecked());
  });

  it("restores close behavior when the daemon rejects an optimistic update", async () => {
    const client = new FailingPreferencesClient();
    render(<DesktopClientProvider client={client}><MemoryRouter initialEntries={["/settings"]}><App /></MemoryRouter></DesktopClientProvider>);

    fireEvent.click(await screen.findByRole("button", { name: "General" }));
    const toggle = await screen.findByRole("switch", { name: "Keep running in notification area" });
    fireEvent.click(toggle);

    expect(await screen.findByRole("alert")).toHaveTextContent("previous setting has been restored");
    expect(toggle).toBeChecked();
  });
});

import { fireEvent, render, screen, within } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { describe, expect, it } from "vitest";
import { DesktopClientProvider } from "../services/DesktopClientContext";
import type { DesktopSnapshot, RunSummary } from "../services/desktopClient";
import { initialSnapshot } from "../services/mockData";
import { MockDesktopClient } from "../services/mockDesktopClient";
import { GROK_EXECUTION_UNAVAILABLE_REASON } from "../services/productAvailability";
import { ActivityView } from "./ActivityView";

class SnapshotClient extends MockDesktopClient {
  constructor(private readonly activitySnapshot: DesktopSnapshot) {
    super();
  }

  override async getSnapshot(): Promise<DesktopSnapshot> {
    return structuredClone(this.activitySnapshot);
  }
}

class PendingSnapshotClient extends MockDesktopClient {
  override async getSnapshot(): Promise<DesktopSnapshot> {
    return new Promise<DesktopSnapshot>(() => undefined);
  }
}

function renderActivity(client: MockDesktopClient = new MockDesktopClient(), initialEntry = "/activity") {
  render(
    <DesktopClientProvider client={client}>
      <MemoryRouter initialEntries={[initialEntry]}>
        <ActivityView />
      </MemoryRouter>
    </DesktopClientProvider>,
  );
  return client;
}

function interruptedRun(): RunSummary {
  return {
    id: "run-interrupted",
    title: "Publish interrupted release notes",
    projectName: "Atlas launch",
    state: "interrupted_needs_review",
    progress: 63,
    updatedAt: "8 min",
    detail: "The write boundary was interrupted before completion could be confirmed",
    steps: [
      { label: "Prepare release notes", state: "done" },
      { label: "Write approved file", state: "active" },
      { label: "Confirm final state", state: "waiting" },
    ],
  };
}

describe("ActivityView", () => {
  it("reserves the run layout while daemon-owned activity is loading", () => {
    renderActivity(new PendingSnapshotClient());

    expect(screen.getByRole("status", { name: "Loading activity" })).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Runs" })).toHaveAttribute("aria-busy", "true");
  });

  it("shows an honest empty state when no runs match", async () => {
    const snapshot = structuredClone(initialSnapshot);
    snapshot.runs = [];
    renderActivity(new SnapshotClient(snapshot));

    expect(await screen.findByRole("heading", { name: "Nothing here" })).toBeInTheDocument();
    expect(screen.getByText("No work matches this filter.")).toBeInTheDocument();
  });

  it("selects the run requested by a project activity link", async () => {
    renderActivity(new MockDesktopClient(), "/activity?run=run-3");

    expect(await screen.findByRole("heading", { name: "Reconcile vendor invoices" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Reconcile vendor invoices/ })).toHaveAttribute("aria-pressed", "true");
  });

  it("supports keyboard filters and counts interrupted runs as needing input", async () => {
    const snapshot = structuredClone(initialSnapshot);
    snapshot.runs.push(interruptedRun());
    renderActivity(new SnapshotClient(snapshot));

    const tabs = await screen.findByRole("tablist", { name: "Activity filters" });
    const allTab = within(tabs).getByRole("tab", { name: "All" });
    const needsInputTab = within(tabs).getByRole("tab", { name: "Needs input, 2" });
    expect(allTab).toHaveAttribute("aria-selected", "true");
    expect(needsInputTab).toHaveTextContent("2");

    allTab.focus();
    fireEvent.keyDown(allTab, { key: "ArrowRight" });
    expect(needsInputTab).toHaveFocus();
    expect(needsInputTab).toHaveAttribute("aria-selected", "true");
    expect(screen.getByRole("button", { name: /Publish the Friday operations brief/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Publish interrupted release notes/ })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /Publish interrupted release notes/ }));
    expect(screen.getByRole("heading", { name: "Manual review required" })).toBeInTheDocument();
    expect(screen.getByText(/will not replay automatically/i)).toBeInTheDocument();

    fireEvent.keyDown(needsInputTab, { key: "End" });
    const completedTab = within(tabs).getByRole("tab", { name: "Completed" });
    expect(completedTab).toHaveFocus();
    expect(completedTab).toHaveAttribute("aria-selected", "true");
    expect(screen.getByRole("button", { name: /Summarize customer interviews/ })).toBeInTheDocument();
    expect(screen.queryByText("Publish interrupted release notes")).not.toBeInTheDocument();

    fireEvent.keyDown(completedTab, { key: "Home" });
    expect(allTab).toHaveFocus();
    expect(allTab).toHaveAttribute("aria-selected", "true");
  });

  it("keeps execution and approval mutations unavailable", async () => {
    const client = new MockDesktopClient();
    renderActivity(client);

    const pause = await screen.findByRole("button", { name: "Pause unavailable" });
    expect(pause).toBeDisabled();
    expect(pause).toHaveAttribute("title", GROK_EXECUTION_UNAVAILABLE_REASON);
    fireEvent.click(pause);

    fireEvent.click(screen.getByRole("button", { name: /Publish the Friday operations brief/ }));
    const decline = screen.getByRole("button", { name: "Decline unavailable" });
    const approve = screen.getByRole("button", { name: "Approve once unavailable" });
    expect(decline).toBeDisabled();
    expect(approve).toBeDisabled();
    expect(decline).toHaveAttribute("title", GROK_EXECUTION_UNAVAILABLE_REASON);
    expect(approve).toHaveAttribute("title", GROK_EXECUTION_UNAVAILABLE_REASON);
    expect(screen.getAllByText(GROK_EXECUTION_UNAVAILABLE_REASON)).not.toHaveLength(0);
    fireEvent.click(decline);
    fireEvent.click(approve);

    fireEvent.click(screen.getByRole("button", { name: /Reconcile vendor invoices/ }));
    const resume = screen.getByRole("button", { name: "Resume unavailable" });
    expect(resume).toBeDisabled();
    expect(resume).toHaveAttribute("title", GROK_EXECUTION_UNAVAILABLE_REASON);
    fireEvent.click(resume);
  });
});

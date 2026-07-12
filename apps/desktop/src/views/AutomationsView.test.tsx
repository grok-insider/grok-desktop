import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { describe, expect, it } from "vitest";
import { DesktopClientProvider } from "../services/DesktopClientContext";
import type { AutomationDraft, AutomationSummary, ClientResult, DesktopSnapshot } from "../services/desktopClient";
import { initialSnapshot } from "../services/mockData";
import { MockDesktopClient } from "../services/mockDesktopClient";
import { AutomationsView } from "./AutomationsView";

class SnapshotClient extends MockDesktopClient {
  constructor(private readonly value: DesktopSnapshot) {
    super();
  }

  override async getSnapshot(): Promise<DesktopSnapshot> {
    return structuredClone(this.value);
  }
}

class PendingSnapshotClient extends MockDesktopClient {
  override async getSnapshot(): Promise<DesktopSnapshot> {
    return new Promise<DesktopSnapshot>(() => undefined);
  }
}

class RejectingAutomationClient extends MockDesktopClient {
  readonly saveCalls: AutomationDraft[] = [];

  override async saveAutomation(draft: AutomationDraft): Promise<ClientResult<AutomationSummary>> {
    this.saveCalls.push(structuredClone(draft));
    return { status: "unavailable", reason: "The daemon rejected this definition." };
  }
}

function renderAutomations(client: MockDesktopClient = new MockDesktopClient()) {
  render(
    <DesktopClientProvider client={client}>
      <MemoryRouter>
        <AutomationsView />
      </MemoryRouter>
    </DesktopClientProvider>,
  );
  return client;
}

function completeRequiredEditorFields(dialog: HTMLElement) {
  fireEvent.change(within(dialog).getByLabelText("Name"), { target: { value: "  Release readiness scan  " } });
  fireEvent.change(within(dialog).getByLabelText("Task prompt"), { target: { value: "  Review launch blockers  " } });
}

describe("AutomationsView", () => {
  it.each([
    ["kernel_initialized_execution_disabled", "Kernel initialized"],
    ["kernel_initialized_execution_enabled", "Execution enabled"],
    ["recovery_pending_execution_disabled", "Recovering"],
    ["degraded_execution_disabled", "Unavailable"],
  ] as const)("maps scheduler health %s", async (state, label) => {
    const snapshot = structuredClone(initialSnapshot);
    snapshot.connection.automationScheduler = { state };
    if (state === "kernel_initialized_execution_enabled") {
      snapshot.capabilities = snapshot.capabilities.map((capability) =>
        capability.id === "automations"
          ? { ...capability, available: true, availability: "available" as const, reason: "Available." }
          : capability,
      );
    }
    renderAutomations(new SnapshotClient(snapshot));

    const metric = (await screen.findByText("Scheduler status")).closest("div");
    expect(metric).toHaveTextContent(label);
    if (state === "kernel_initialized_execution_enabled") {
      expect(screen.getByText("Scheduler armed")).toBeInTheDocument();
    } else {
      expect(screen.getByText("Definitions are inactive")).toBeInTheDocument();
      for (const toggle of screen.getAllByRole("switch")) {
        expect(toggle).toBeDisabled();
        expect(toggle).not.toBeChecked();
      }
    }
  });

  it("reserves the definitions layout while the daemon snapshot is loading", () => {
    renderAutomations(new PendingSnapshotClient());

    expect(screen.getByRole("status", { name: "Loading automation definitions" })).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Saved definitions" })).toHaveAttribute("aria-busy", "true");
    expect(screen.getByRole("button", { name: "New definition" })).toBeDisabled();
  });

  it("shows an honest empty state and blocks creation when no project exists", async () => {
    const snapshot = structuredClone(initialSnapshot);
    snapshot.automations = [];
    snapshot.projects = [];
    renderAutomations(new SnapshotClient(snapshot));

    expect(await screen.findByText("No definitions yet")).toBeInTheDocument();
    expect(screen.getByText("Create a project before adding a definition.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "New definition" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "New definition" })).toHaveAttribute(
      "title",
      "Create a project before adding a definition.",
    );
  });

  it("renders daemon-reported schedule state without inventing local next runs", async () => {
    const snapshot = structuredClone(initialSnapshot);
    snapshot.automations[0] = {
      ...snapshot.automations[0],
      enabled: true,
      nextRun: "Scheduled by daemon",
    };
    snapshot.connection.automationScheduler = {
      state: "kernel_initialized_execution_enabled",
    };
    snapshot.capabilities = snapshot.capabilities.map((capability) =>
      capability.id === "automations"
        ? { ...capability, available: true, availability: "available" as const, reason: "Available." }
        : capability,
    );
    renderAutomations(new SnapshotClient(snapshot));

    expect(await screen.findByRole("button", { name: "View Friday operations brief" })).toBeInTheDocument();
    expect(screen.getByText("Fri at 4:00 PM")).toBeInTheDocument();
    expect(screen.getByText("Scheduled by daemon")).toBeInTheDocument();
    expect(screen.getAllByText("Not scheduled").length).toBeGreaterThanOrEqual(2);
    expect(screen.getByText("Scheduler armed")).toBeInTheDocument();
    expect(screen.getByRole("switch", { name: "Friday operations brief is enabled" })).toBeChecked();
    expect(screen.getByRole("switch", { name: "Friday operations brief is enabled" })).toBeDisabled();

    fireEvent.click(screen.getByRole("button", { name: "View Friday operations brief" }));
    const detail = await screen.findByRole("dialog", { name: "Friday operations brief" });
    expect(within(detail).getByRole("button", { name: "Run unavailable" })).toBeDisabled();
  });

  it("shows unavailable legacy schedules without offering a fabricated edit", async () => {
    const snapshot = structuredClone(initialSnapshot);
    snapshot.automations[0] = {
      ...snapshot.automations[0],
      schedule: "Schedule unavailable",
      scheduleConfig: undefined,
    };
    renderAutomations(new SnapshotClient(snapshot));

    fireEvent.click(await screen.findByRole("button", { name: "View Friday operations brief" }));
    const detail = await screen.findByRole("dialog", { name: "Friday operations brief" });
    expect(within(detail).getByText("Schedule unavailable")).toBeInTheDocument();
    expect(within(detail).queryByText("Daily at 09:00")).not.toBeInTheDocument();
    expect(within(detail).getByRole("button", { name: "Edit definition" })).toBeDisabled();
    expect(within(detail).getByRole("button", { name: "Edit definition" })).toHaveAttribute(
      "title",
      "This schedule cannot be edited safely. Create a new definition instead.",
    );
  });

  it("keeps project ownership read-only while editing an existing definition", async () => {
    const client = new RejectingAutomationClient();
    renderAutomations(client);
    fireEvent.click(await screen.findByRole("button", { name: "View Friday operations brief" }));
    fireEvent.click(within(await screen.findByRole("dialog", { name: "Friday operations brief" })).getByRole(
      "button",
      { name: "Edit definition" },
    ));
    const editor = await screen.findByRole("dialog", { name: "Edit definition" });
    const project = within(editor).getByLabelText("Project");
    expect(project).toHaveAttribute("readonly");
    expect(project).toHaveAccessibleDescription("Project ownership cannot be changed after creation.");
    expect(project).toHaveValue("Operations");

    fireEvent.change(project, { target: { value: "Atlas launch" } });
    fireEvent.click(within(editor).getByRole("button", { name: "Save definition" }));

    expect(await within(editor).findByRole("alert")).toBeInTheDocument();
    expect(client.saveCalls[0]).toMatchObject({ id: "auto-1", projectId: "ops" });
  });

  it("validates the editor, focuses the first field, and restores focus after Escape", async () => {
    renderAutomations();
    const trigger = await screen.findByRole("button", { name: "New definition" });
    trigger.focus();
    fireEvent.click(trigger);

    const dialog = await screen.findByRole("dialog", { name: "New definition" });
    const name = within(dialog).getByLabelText("Name");
    const save = within(dialog).getByRole("button", { name: "Save definition" });
    expect(name).toHaveFocus();
    expect(save).toBeDisabled();
    expect(within(dialog).getByRole("switch", { name: "Definition is inactive" })).toBeDisabled();

    fireEvent.change(name, { target: { value: "Release readiness scan" } });
    expect(save).toBeDisabled();
    fireEvent.change(within(dialog).getByLabelText("Task prompt"), { target: { value: "Review launch blockers" } });
    expect(save).toBeEnabled();

    fireEvent.keyDown(document, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "New definition" })).not.toBeInTheDocument());
    await waitFor(() => expect(trigger).toHaveFocus());
  });

  it("forces every saved draft inactive", async () => {
    const client = renderAutomations();
    const trigger = await screen.findByRole("button", { name: "New definition" });
    fireEvent.click(trigger);
    const dialog = await screen.findByRole("dialog", { name: "New definition" });
    completeRequiredEditorFields(dialog);
    fireEvent.click(within(dialog).getByRole("button", { name: "Save definition" }));

    expect(await screen.findByText("Definition saved inactive.")).toBeInTheDocument();
    const saved = (await client.getSnapshot()).automations.find((item) => item.name === "Release readiness scan");
    expect(saved).toMatchObject({
      name: "Release readiness scan",
      prompt: "Review launch blockers",
      enabled: false,
      nextRun: "Not scheduled",
    });
  });

  it("keeps the editor usable when the daemon rejects a definition", async () => {
    const client = new RejectingAutomationClient();
    renderAutomations(client);
    fireEvent.click(await screen.findByRole("button", { name: "New definition" }));
    const dialog = await screen.findByRole("dialog", { name: "New definition" });
    completeRequiredEditorFields(dialog);
    fireEvent.click(within(dialog).getByRole("button", { name: "Save definition" }));

    expect(await within(dialog).findByRole("alert")).toHaveTextContent("The daemon rejected this definition.");
    expect(within(dialog).getByLabelText("Name")).toHaveValue("  Release readiness scan  ");
    await waitFor(() => expect(within(dialog).getByRole("button", { name: "Save definition" })).toBeEnabled());
    expect(client.saveCalls).toHaveLength(1);
    expect(client.saveCalls[0]).toMatchObject({
      name: "Release readiness scan",
      prompt: "Review launch blockers",
    });
  });
});

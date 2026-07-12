import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { DesktopClientProvider } from "../services/DesktopClientContext";
import type { ClientResult, ManagedIntegrationDetail } from "../services/desktopClient";
import { MockDesktopClient } from "../services/mockDesktopClient";
import { ExtensionsView } from "./ExtensionsView";

class ManagedIntegrationClient extends MockDesktopClient {
  readonly actions: ("install" | "update" | "rollback")[] = [];

  constructor(
    private detail: ManagedIntegrationDetail,
    private readonly rejection?: string,
  ) {
    super();
  }

  override async getManagedIntegration(): Promise<ClientResult<ManagedIntegrationDetail>> {
    return { status: "success", value: structuredClone(this.detail) };
  }

  override async changeManagedIntegration(
    _integrationId: "wisp",
    action: "install" | "update" | "rollback",
  ): Promise<ClientResult<ManagedIntegrationDetail>> {
    this.actions.push(action);
    if (this.rejection) return { status: "unavailable", reason: this.rejection };
    if (action === "install") {
      this.detail = { ...this.detail, state: "installed", installedVersion: this.detail.availableVersion };
    } else if (action === "update") {
      this.detail = {
        ...this.detail,
        state: "rollback_available",
        rollbackVersion: this.detail.installedVersion,
        installedVersion: this.detail.availableVersion,
      };
    } else if (this.detail.rollbackVersion) {
      this.detail = { ...this.detail, state: "update_available", installedVersion: this.detail.rollbackVersion };
    }
    return { status: "success", value: structuredClone(this.detail) };
  }
}

function wispDetail(overrides: Partial<ManagedIntegrationDetail> = {}): ManagedIntegrationDetail {
  return {
    id: "wisp",
    name: "Wisp",
    recommended: true,
    state: "update_available",
    installedVersion: "0.8.2",
    availableVersion: "0.9.0",
    rollbackVersion: "0.8.1",
    checks: [
      { label: "Signed component", state: "ready", detail: "Publisher and manifest verified" },
      { label: "Protocol compatibility", state: "ready", detail: "Compatible with this desktop build" },
    ],
    permissions: ["Observe approved applications", "Send input after scoped approval"],
    releaseNotes: ["Adds signed rollback metadata"],
    ...overrides,
  };
}

function renderExtensions(client = new MockDesktopClient()) {
  render(
    <DesktopClientProvider client={client}>
      <ExtensionsView />
    </DesktopClientProvider>,
  );
  return client;
}

async function openWisp() {
  const trigger = await screen.findByRole("button", { name: "View details" });
  trigger.focus();
  fireEvent.click(trigger);
  return screen.findByRole("dialog", { name: "Wisp" });
}

describe("ExtensionsView", () => {
  it("searches extensions and supports pointer and keyboard filters", async () => {
    renderExtensions();
    expect(await screen.findByRole("heading", { name: "Managed browser", level: 3 })).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("Search extensions"), { target: { value: "Wisp" } });
    expect(screen.getByRole("heading", { name: "Wisp", level: 3 })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Managed browser", level: 3 })).not.toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("Search extensions"), { target: { value: "" } });
    const allTab = screen.getByRole("tab", { name: "All" });
    fireEvent.keyDown(allTab, { key: "ArrowRight" });
    const builtInTab = screen.getByRole("tab", { name: "Built-in" });
    expect(builtInTab).toHaveAttribute("aria-selected", "true");
    expect(builtInTab).toHaveFocus();
    expect(screen.queryByRole("heading", { name: "Wisp", level: 3 })).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("tab", { name: "MCP" }));
    expect(screen.getByRole("heading", { name: "GitHub MCP", level: 3 })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Linear MCP", level: 3 })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Wisp", level: 3 })).not.toBeInTheDocument();
  });

  it("focuses the Wisp detail, closes it with Escape, and restores focus", async () => {
    renderExtensions();
    const trigger = await screen.findByRole("button", { name: "View details" });
    trigger.focus();
    fireEvent.click(trigger);
    const dialog = await screen.findByRole("dialog", { name: "Wisp" });
    const close = within(dialog).getByRole("button", { name: "Close" });
    await waitFor(() => expect(close).toHaveFocus());

    fireEvent.keyDown(document, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Wisp" })).not.toBeInTheDocument());
    await waitFor(() => expect(trigger).toHaveFocus());
  });

  it.each([
    { state: "available" as const, label: "Install Wisp" },
    { state: "update_available" as const, label: "Update to 0.9.0" },
  ])("keeps $state actions unavailable when a readiness check requires action", async ({ state, label }) => {
    const reason = "Installer support is not available in the current daemon protocol";
    const client = new ManagedIntegrationClient(wispDetail({
      state,
      installedVersion: state === "available" ? undefined : "0.8.2",
      rollbackVersion: undefined,
      checks: [{ label: "Managed add-on service", state: "action_required", detail: reason }],
    }));
    renderExtensions(client);
    const dialog = await openWisp();
    const action = within(dialog).getByRole("button", { name: label });
    expect(action).toBeDisabled();
    expect(action).toHaveAttribute("title", reason);
    fireEvent.click(action);
    expect(client.actions).toEqual([]);
  });

  it("surfaces a daemon-rejected update without changing the reported component state", async () => {
    const reason = "Managed add-on installation is not exposed by the current daemon protocol.";
    const client = new ManagedIntegrationClient(wispDetail(), reason);
    renderExtensions(client);
    const dialog = await openWisp();
    fireEvent.click(within(dialog).getByRole("button", { name: "Update to 0.9.0" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(reason);
    expect(client.actions).toEqual(["update"]);
    expect(within(dialog).getByText("Update available")).toBeInTheDocument();
  });

  it("reports signed installation progress without exposing another action", async () => {
    renderExtensions(new ManagedIntegrationClient(wispDetail({
      state: "installing",
      installedVersion: undefined,
      rollbackVersion: undefined,
      progress: 42,
    })));
    const dialog = await openWisp();
    const progress = within(dialog).getByRole("progressbar", { name: "Wisp installation progress" });
    expect(progress).toHaveAttribute("aria-valuenow", "42");
    expect(progress.firstElementChild).toHaveStyle({ transform: "scaleX(0.42)" });
    expect(within(dialog).getByRole("button", { name: "Installing signed component" })).toBeDisabled();
  });

  it("runs the explicitly reported signed rollback and updates its state", async () => {
    const client = new ManagedIntegrationClient(wispDetail({
      state: "rollback_available",
      installedVersion: "0.9.0",
      availableVersion: "0.9.0",
      rollbackVersion: "0.8.2",
    }));
    renderExtensions(client);
    const dialog = await openWisp();
    expect(within(dialog).getByText("Signed rollback available")).toBeInTheDocument();
    fireEvent.click(within(dialog).getByRole("button", { name: "Rollback to 0.8.2" }));

    expect(await screen.findByRole("status")).toHaveTextContent("Wisp rollback completed.");
    expect(client.actions).toEqual(["rollback"]);
    expect(within(dialog).getByText("Update available")).toBeInTheDocument();
  });
});

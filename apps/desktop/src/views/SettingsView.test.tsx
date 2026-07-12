import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { MemoryRouter, useLocation } from "react-router-dom";
import { describe, expect, it, vi } from "vitest";
import { DesktopClientProvider } from "../services/DesktopClientContext";
import type { AccountSetupState } from "../services/desktopClient";
import { MockDesktopClient } from "../services/mockDesktopClient";
import { SettingsView } from "./SettingsView";

class FailingPreferencesClient extends MockDesktopClient {
  override async updateDesktopPreferences(_input: {
    expectedRevision: number;
    keepRunningInNotificationArea: boolean;
  }): Promise<never> {
    throw new Error("daemon unavailable");
  }
}

class FailingAccountClient extends MockDesktopClient {
  override async getAccountSetup(): Promise<AccountSetupState> {
    throw new Error("vault unavailable");
  }
}

function renderSettings(client: MockDesktopClient = new MockDesktopClient()) {
  render(
    <DesktopClientProvider client={client}>
      <MemoryRouter initialEntries={["/settings"]}>
        <SettingsView />
        <LocationProbe />
      </MemoryRouter>
    </DesktopClientProvider>,
  );
  return client;
}

function LocationProbe() {
  const location = useLocation();
  return <span className="hidden" data-testid="current-location">{location.pathname}</span>;
}

describe("SettingsView", () => {
  it("exposes labeled section navigation and supports arrow, Home, and End keys", () => {
    renderSettings();

    const navigation = screen.getByRole("navigation", { name: "Settings sections" });
    const sectionButtons = within(navigation).getAllByRole("button");
    expect(sectionButtons).toHaveLength(3);
    sectionButtons.forEach((button) => expect(button).toHaveAttribute("aria-controls", "settings-panel"));

    const accountButton = within(navigation).getByRole("button", { name: "Account" });
    expect(accountButton).toHaveAttribute("aria-current", "page");
    expect(screen.getByRole("region", { name: "Account" })).toHaveAttribute("id", "settings-panel");

    accountButton.focus();
    fireEvent.keyDown(accountButton, { key: "ArrowDown" });

    const generalButton = within(navigation).getByRole("button", { name: "General" });
    expect(generalButton).toHaveFocus();
    expect(generalButton).toHaveAttribute("aria-current", "page");
    expect(screen.getByRole("region", { name: "General" })).toBeInTheDocument();

    fireEvent.keyDown(generalButton, { key: "End" });
    const updatesButton = within(navigation).getByRole("button", { name: "Models" });
    expect(updatesButton).toHaveFocus();
    expect(screen.getByRole("region", { name: "Models" })).toBeInTheDocument();

    fireEvent.keyDown(updatesButton, { key: "Home" });
    expect(accountButton).toHaveFocus();
    expect(screen.getByRole("region", { name: "Account" })).toBeInTheDocument();
  });

  it("saves daemon-owned close behavior with the loaded revision", async () => {
    const client = new MockDesktopClient();
    const update = vi.spyOn(client, "updateDesktopPreferences");
    renderSettings(client);

    fireEvent.click(screen.getByRole("button", { name: "General" }));
    const toggle = await screen.findByRole("switch", { name: "Keep running in notification area" });
    await waitFor(() => expect(toggle).toBeEnabled());
    expect(toggle).toBeChecked();

    fireEvent.click(toggle);

    await waitFor(() => {
      expect(update).toHaveBeenCalledWith({
        expectedRevision: 0,
        keepRunningInNotificationArea: false,
      });
    });
    await waitFor(() => expect(toggle).not.toBeChecked());
  });

  it("rolls back an optimistic close-behavior update and announces the error", async () => {
    renderSettings(new FailingPreferencesClient());

    fireEvent.click(screen.getByRole("button", { name: "General" }));
    const toggle = await screen.findByRole("switch", { name: "Keep running in notification area" });
    await waitFor(() => expect(toggle).toBeEnabled());
    fireEvent.click(toggle);

    expect(await screen.findByRole("alert")).toHaveTextContent("previous setting has been restored");
    expect(toggle).toBeChecked();
    expect(toggle).toBeEnabled();
  });

  it("reports a configured vault credential without exposing its value", async () => {
    renderSettings();

    expect(await screen.findByText("Configured")).toBeInTheDocument();
    expect(screen.getByText("Stored in the operating system credential vault")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Manage" })).toBeEnabled();
    expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
  });

  it("offers secure enrollment when the daemon reports no configured key", async () => {
    renderSettings(new MockDesktopClient({ firstRun: true }));

    expect(await screen.findByText("Not configured")).toBeInTheDocument();
    expect(screen.getByText("Add a key through the secure operating system credential prompt")).toBeInTheDocument();
    const addKey = screen.getByRole("button", { name: "Add key" });
    expect(addKey).toBeEnabled();

    fireEvent.click(addKey);
    expect(screen.getByTestId("current-location")).toHaveTextContent("/setup");
  });

  it("announces credential-status failures and keeps the recovery route available", async () => {
    renderSettings(new FailingAccountClient());

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("Credential status unavailable");
    expect(alert).toHaveTextContent("Open setup to retry");
    expect(screen.getByRole("button", { name: "Retry" })).toBeEnabled();
  });

  it("only shows daemon-backed close behavior in General", async () => {
    renderSettings();
    fireEvent.click(screen.getByRole("button", { name: "General" }));
    expect(await screen.findByRole("switch", { name: "Keep running in notification area" })).toBeInTheDocument();
    expect(screen.queryByRole("radiogroup", { name: "Appearance" })).not.toBeInTheDocument();
    expect(screen.queryByRole("switch", { name: "Launch at sign in unavailable" })).not.toBeInTheDocument();
  });

  it("selects only live text-ready daemon models", async () => {
    const client = new MockDesktopClient();
    const selectModel = vi.spyOn(client, "selectChatModel");
    renderSettings(client);

    fireEvent.click(screen.getByRole("button", { name: "Models" }));
    const modelSelect = await screen.findByRole("combobox", { name: "Default chat model" });
    await waitFor(() => expect(modelSelect).toBeEnabled());
    expect(modelSelect).toHaveValue("grok-4.3");
    expect(within(modelSelect).getByRole("option", { name: "grok-4.3 — product default" })).toBeInTheDocument();

    fireEvent.change(modelSelect, { target: { value: "grok-4.3-fast" } });

    await waitFor(() => expect(selectModel).toHaveBeenCalledWith({
      expectedRevision: 0,
      modelId: "grok-4.3-fast",
    }));
    await waitFor(() => expect(modelSelect).toHaveValue("grok-4.3-fast"));
    expect(screen.getByLabelText("Model status")).toHaveTextContent("existing turns never change");
  });

  it("shows an honest retry state when live official model discovery fails", async () => {
    class FailingModelsClient extends MockDesktopClient {
      override async getChatModelCatalog(): Promise<never> {
        throw new Error("provider unavailable");
      }
    }
    renderSettings(new FailingModelsClient());

    fireEvent.click(screen.getByRole("button", { name: "Models" }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("Model catalog unavailable");
    expect(alert).toHaveTextContent("Check the API key and network");
    expect(screen.getByRole("button", { name: "Retry discovery" })).toBeEnabled();
    expect(screen.getByRole("combobox", { name: "Default chat model" })).toBeDisabled();
  });

  it("keeps model discovery unavailable in first-run preview without an xAI key", async () => {
    renderSettings(new MockDesktopClient({ firstRun: true }));
    fireEvent.click(screen.getByRole("button", { name: "Models" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("Model catalog unavailable");
    expect(screen.getByRole("combobox", { name: "Default chat model" })).toBeDisabled();
  });

  it("invalidates the catalog when a selection outcome needs reconciliation", async () => {
    class SelectionFailureClient extends MockDesktopClient {
      override async selectChatModel(): Promise<never> {
        throw new Error("readiness reconciliation failed");
      }
    }
    renderSettings(new SelectionFailureClient());
    fireEvent.click(screen.getByRole("button", { name: "Models" }));
    const modelSelect = await screen.findByRole("combobox", { name: "Default chat model" });
    await waitFor(() => expect(modelSelect).toBeEnabled());

    fireEvent.change(modelSelect, { target: { value: "grok-4.3-fast" } });

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("could not be reconciled");
    expect(screen.getByRole("button", { name: "Retry discovery" })).toBeEnabled();
    expect(screen.getByRole("combobox", { name: "Default chat model" })).toBeDisabled();
    expect(screen.getByLabelText("Model status")).not.toHaveTextContent("Ready");
  });

  it("invalidates a previously ready catalog when explicit live refresh fails", async () => {
    class RefreshFailureClient extends MockDesktopClient {
      private discoveryCalls = 0;

      override async getChatModelCatalog() {
        this.discoveryCalls += 1;
        if (this.discoveryCalls > 1) throw new Error("provider unavailable");
        return super.getChatModelCatalog();
      }
    }
    renderSettings(new RefreshFailureClient());
    fireEvent.click(screen.getByRole("button", { name: "Models" }));
    const modelSelect = await screen.findByRole("combobox", { name: "Default chat model" });
    await waitFor(() => expect(modelSelect).toBeEnabled());
    expect(screen.getByLabelText("Model status")).toHaveTextContent("Ready");

    fireEvent.click(screen.getByRole("button", { name: "Refresh" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("Model catalog unavailable");
    expect(screen.getByRole("combobox", { name: "Default chat model" })).toBeDisabled();
    expect(screen.getByLabelText("Model status")).not.toHaveTextContent("Ready");
  });
});

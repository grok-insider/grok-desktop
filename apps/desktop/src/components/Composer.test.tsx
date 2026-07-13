import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter, useLocation } from "react-router-dom";
import { describe, expect, it, vi } from "vitest";
import { DesktopClientProvider } from "../services/DesktopClientContext";
import type { StartRunInput } from "../services/desktopClient";
import { MockDesktopClient } from "../services/mockDesktopClient";
import { Composer } from "./Composer";

class CapturingClient extends MockDesktopClient {
  readonly starts: Array<StartRunInput & { modelId?: string }> = [];
  failStart = false;

  override async startRun(input: StartRunInput & { modelId?: string }) {
    this.starts.push(structuredClone(input));
    if (this.failStart) throw new Error("Provider start failed.");
    return super.startRun(input);
  }
}

function renderComposer(client: MockDesktopClient = new CapturingClient()) {
  render(
    <DesktopClientProvider client={client}>
      <MemoryRouter>
        <Composer />
        <LocationProbe />
      </MemoryRouter>
    </DesktopClientProvider>,
  );
  return client;
}

function LocationProbe() {
  const location = useLocation();
  return <span className="hidden" data-testid="current-location">{location.pathname}{location.search}</span>;
}

async function chooseModel(modelId: string) {
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: /Choose model/ }));
  // Accessible name includes product label and mono canonical id.
  await user.click(await screen.findByRole("menuitemradio", { name: new RegExp(modelId) }));
}

describe("Composer Imagine tools", () => {
  it("does not probe the provider catalog while daemon Chat is unavailable", async () => {
    class NoChatClient extends CapturingClient {
      override async getSnapshot() {
        const snapshot = await super.getSnapshot();
        return {
          ...snapshot,
          capabilities: snapshot.capabilities.map((capability) => capability.id === "chat"
            ? { ...capability, available: false, reason: "Connect an account first." }
            : capability),
        };
      }
    }
    const client = new NoChatClient();
    const discovery = vi.spyOn(client, "getChatModelCatalog");
    renderComposer(client);

    expect(await screen.findByRole("button", { name: "Enable Search" })).toBeDisabled();
    expect(discovery).not.toHaveBeenCalled();
  });

  it("gates Imagine tools when capabilities are unavailable", async () => {
    class NoImagineClient extends CapturingClient {
      override async getSnapshot() {
        const snapshot = await super.getSnapshot();
        return {
          ...snapshot,
          capabilities: snapshot.capabilities.map((capability) =>
            capability.id === "imagine_image" || capability.id === "imagine_video"
              ? { ...capability, available: false }
              : capability,
          ),
        };
      }
    }
    renderComposer(new NoImagineClient());
    const tools = await screen.findByRole("button", { name: "Tools" });
    expect(tools).toBeDisabled();
  });

  it("starts Imagine media without changing the selected chat model", async () => {
    const client = renderComposer() as CapturingClient;
    const createMedia = vi.spyOn(client, "createMedia");
    await chooseModel("grok-4.3-fast");
    expect(screen.getByRole("button", { name: "Choose model, Grok 4.3 Fast" })).toBeInTheDocument();

    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "Tools" }));
    await user.click(await screen.findByRole("menuitem", { name: "Imagine image" }));
    expect(screen.getByLabelText("Imagine image tool")).toBeInTheDocument();

    const mediaPrompt = screen.getByLabelText("Media prompt");
    fireEvent.change(mediaPrompt, { target: { value: "A charcoal studio still life" } });
    // Chat model must still be the override after opening the tool.
    expect(screen.getByRole("button", { name: "Choose model, Grok 4.3 Fast" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Create" }));
    await waitFor(() => expect(createMedia).toHaveBeenCalledWith({
      kind: "image",
      prompt: "A charcoal studio still life",
      aspectRatio: "1:1",
    }));
    // Selecting Imagine must not clear or change the chat model override.
    expect(screen.getByRole("button", { name: "Choose model, Grok 4.3 Fast" })).toBeInTheDocument();
    expect(client.starts).toHaveLength(0);
  });
});

describe("Composer model selection", () => {
  it("routes unavailable Work directly to its settings section", async () => {
    class NoWorkClient extends CapturingClient {
      override async getSnapshot() {
        const snapshot = await super.getSnapshot();
        return {
          ...snapshot,
          capabilities: snapshot.capabilities.map((capability) => capability.id === "work"
            ? { ...capability, available: false, reason: "Choose an execution mode." }
            : capability),
          workExecution: { ...snapshot.workExecution, mode: "limited" as const },
        };
      }
    }
    const user = userEvent.setup();
    renderComposer(new NoWorkClient());

    await user.click(await screen.findByRole("tab", { name: "Work" }));
    await user.click(screen.getByRole("button", { name: "Work settings" }));

    expect(screen.getByTestId("current-location")).toHaveTextContent("/settings?section=work");
  });

  it("binds the official Search preset to the submitted turn", async () => {
    const client = renderComposer() as CapturingClient;
    fireEvent.click(await screen.findByRole("button", { name: "Enable Search" }));
    expect(screen.getByRole("button", { name: "Disable Search" })).toHaveAttribute("aria-pressed", "true");
    fireEvent.change(screen.getByLabelText("Message Grok"), { target: { value: "What changed today?" } });
    fireEvent.click(screen.getByRole("button", { name: "Send message" }));

    await waitFor(() => expect(client.starts).toHaveLength(1));
    expect(client.starts[0]).toMatchObject({ mode: "chat", searchEnabled: true });
  });

  it("applies a temporary model to one new conversation and clears it after success", async () => {
    const client = renderComposer() as CapturingClient;
    await chooseModel("grok-4.3-fast");
    expect(screen.getByRole("button", { name: "Choose model, Grok 4.3 Fast" })).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("Message Grok"), { target: { value: "Explain this plan" } });
    fireEvent.click(screen.getByRole("button", { name: "Send message" }));

    await waitFor(() => expect(client.starts).toHaveLength(1));
    expect(client.starts[0]).toMatchObject({ modelId: "grok-4.3-fast", mode: "chat" });
    await screen.findByRole("button", { name: "Choose model, Default · Grok 4.3" });
  });

  it("retains the temporary choice when provider start fails", async () => {
    const client = renderComposer() as CapturingClient;
    client.failStart = true;
    await chooseModel("grok-4.3-fast");
    fireEvent.change(screen.getByLabelText("Message Grok"), { target: { value: "Keep my choice" } });
    fireEvent.click(screen.getByRole("button", { name: "Send message" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("Provider start failed.");
    expect(screen.getByRole("button", { name: "Choose model, Grok 4.3 Fast" })).toBeInTheDocument();
    expect(screen.getByLabelText("Message Grok")).toHaveValue("Keep my choice");
  });

  it("saves a model as the daemon-owned default through the secondary action", async () => {
    const client = renderComposer();
    const select = vi.spyOn(client, "selectChatModel");
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: /Choose model/ }));
    await user.click(await screen.findByRole("menuitem", { name: "Set Grok 4.3 Fast as default" }));

    await waitFor(() => expect(select).toHaveBeenCalledWith({ expectedRevision: 0, modelId: "grok-4.3-fast" }));
    await screen.findByRole("button", { name: "Choose model, Default · Grok 4.3 Fast" });
  });

  it("shows bounded discovery failure and retries without disabling the composer", async () => {
    class RetryClient extends CapturingClient {
      calls = 0;
      override async getChatModelCatalog() {
        this.calls += 1;
        if (this.calls === 1) throw new Error("offline");
        return super.getChatModelCatalog();
      }
    }
    const client = renderComposer(new RetryClient()) as RetryClient;
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "Choose model, Choose model" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("model catalog is unavailable");
    await user.click(screen.getByRole("button", { name: "Retry" }));

    await screen.findByRole("menuitemradio", { name: /Default · Grok 4\.3/ });
    expect(client.calls).toBe(2);
  });

  it("closes the accessible menu with Escape", async () => {
    renderComposer();
    const user = userEvent.setup();
    const trigger = await screen.findByRole("button", { name: /Choose model/ });
    await user.click(trigger);
    await screen.findByRole("menu", { name: "Chat model" });
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("menu", { name: "Chat model" })).not.toBeInTheDocument();
    await waitFor(() => expect(trigger).toHaveFocus());
  });
});

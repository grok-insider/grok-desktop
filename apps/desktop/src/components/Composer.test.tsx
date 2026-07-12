import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
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
      </MemoryRouter>
    </DesktopClientProvider>,
  );
  return client;
}

async function chooseModel(modelId: string) {
  fireEvent.click(await screen.findByRole("button", { name: /Choose model/ }));
  fireEvent.click(await screen.findByRole("menuitemradio", { name: modelId }));
}

describe("Composer model selection", () => {
  it("applies a temporary model to one new conversation and clears it after success", async () => {
    const client = renderComposer() as CapturingClient;
    await chooseModel("grok-4.3-fast");
    expect(screen.getByRole("button", { name: "Choose model, grok-4.3-fast" })).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("Message Grok"), { target: { value: "Explain this plan" } });
    fireEvent.click(screen.getByRole("button", { name: "Send message" }));

    await waitFor(() => expect(client.starts).toHaveLength(1));
    expect(client.starts[0]).toMatchObject({ modelId: "grok-4.3-fast", mode: "chat" });
    await screen.findByRole("button", { name: "Choose model, Default · grok-4.3" });
  });

  it("retains the temporary choice when provider start fails", async () => {
    const client = renderComposer() as CapturingClient;
    client.failStart = true;
    await chooseModel("grok-4.3-fast");
    fireEvent.change(screen.getByLabelText("Message Grok"), { target: { value: "Keep my choice" } });
    fireEvent.click(screen.getByRole("button", { name: "Send message" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("Provider start failed.");
    expect(screen.getByRole("button", { name: "Choose model, grok-4.3-fast" })).toBeInTheDocument();
    expect(screen.getByLabelText("Message Grok")).toHaveValue("Keep my choice");
  });

  it("saves a model as the daemon-owned default through the secondary action", async () => {
    const client = renderComposer();
    const select = vi.spyOn(client, "selectChatModel");
    fireEvent.click(await screen.findByRole("button", { name: /Choose model/ }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Set grok-4.3-fast as default" }));

    await waitFor(() => expect(select).toHaveBeenCalledWith({ expectedRevision: 0, modelId: "grok-4.3-fast" }));
    await screen.findByRole("button", { name: "Choose model, Default · grok-4.3-fast" });
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
    fireEvent.click(await screen.findByRole("button", { name: "Choose model, Choose model" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("model catalog is unavailable");
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));

    await screen.findByRole("menuitemradio", { name: "Default · grok-4.3" });
    expect(client.calls).toBe(2);
  });

  it("closes the accessible menu with Escape", async () => {
    renderComposer();
    const trigger = await screen.findByRole("button", { name: /Choose model/ });
    fireEvent.click(trigger);
    await screen.findByRole("menu", { name: "Chat model" });
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("menu", { name: "Chat model" })).not.toBeInTheDocument();
    await waitFor(() => expect(trigger).toHaveFocus());
  });
});

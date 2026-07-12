import { fireEvent, render, screen, within } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { describe, expect, it } from "vitest";
import { App } from "../app/App";
import { DesktopClientProvider } from "../services/DesktopClientContext";
import type { AccountSetupState } from "../services/desktopClient";
import { MockDesktopClient } from "../services/mockDesktopClient";

function renderSetup(client: MockDesktopClient = new MockDesktopClient()) {
  render(
    <DesktopClientProvider client={client}>
      <MemoryRouter initialEntries={["/setup"]}>
        <App />
      </MemoryRouter>
    </DesktopClientProvider>,
  );
}

class PendingSetupClient extends MockDesktopClient {
  override async getAccountSetup(): Promise<AccountSetupState> {
    return new Promise<AccountSetupState>(() => undefined);
  }
}

describe("SetupView", () => {
  it("moves through the account, API key, and readiness steps", async () => {
    renderSetup();

    const grokStep = screen.getByRole("button", { name: /Grok account/ });
    expect(grokStep).toHaveAttribute("aria-current", "step");
    expect(await screen.findByText("Grok account connected")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Continue in Limited Mode" }));
    expect(screen.getByRole("heading", { name: "xAI API key" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /xAI API key/ })).toHaveAttribute("aria-current", "step");

    fireEvent.click(screen.getByRole("button", { name: "Continue to readiness" }));
    expect(screen.getByRole("heading", { name: "Capability readiness" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Readiness/ })).toHaveAttribute("aria-current", "step");
    expect(screen.getByRole("list")).toBeInTheDocument();

    fireEvent.click(grokStep);
    expect(screen.getByRole("heading", { name: "Connect Grok Build" })).toBeInTheDocument();
  });

  it("keeps the configured key when removal is cancelled", async () => {
    renderSetup();

    fireEvent.click(screen.getByRole("button", { name: /xAI API key/ }));
    expect(await screen.findByText("xAI API key configured")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Remove key" }));

    const dialog = screen.getByRole("dialog", { name: "Remove xAI API key?" });
    fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));

    expect(screen.queryByRole("dialog", { name: "Remove xAI API key?" })).not.toBeInTheDocument();
    expect(screen.getByText("xAI API key configured")).toBeInTheDocument();
  });

  it("reserves the readiness layout while account state is loading", () => {
    renderSetup(new PendingSetupClient());

    fireEvent.click(screen.getByRole("button", { name: /Readiness/ }));
    expect(screen.getByRole("status", { name: "Loading capability readiness" })).toBeInTheDocument();
  });
});

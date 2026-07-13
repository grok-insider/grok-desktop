import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { DesktopClientProvider } from "../services/DesktopClientContext";
import { MockDesktopClient } from "../services/mockDesktopClient";
import { ExtensionsView } from "./ExtensionsView";

function renderExtensions(client = new MockDesktopClient()) {
  render(
    <DesktopClientProvider client={client}>
      <ExtensionsView />
    </DesktopClientProvider>,
  );
  return client;
}

describe("ExtensionsView", () => {
  it("does not advertise Wisp install as a product surface", async () => {
    renderExtensions();
    expect(await screen.findByRole("heading", { name: "Managed browser", level: 3 })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Wisp", level: 3 })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "View details" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Install Wisp/i })).not.toBeInTheDocument();
    expect(screen.getByText(/Managed add-on install is not available/i)).toBeInTheDocument();
  });

  it("searches extensions and supports pointer and keyboard filters", async () => {
    renderExtensions();
    expect(await screen.findByRole("heading", { name: "Managed browser", level: 3 })).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("Search extensions"), { target: { value: "GitHub" } });
    expect(screen.getByRole("heading", { name: "GitHub MCP", level: 3 })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Managed browser", level: 3 })).not.toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("Search extensions"), { target: { value: "" } });
    const allTab = screen.getByRole("tab", { name: "All" });
    allTab.focus();
    fireEvent.keyDown(allTab, { key: "ArrowRight" });
    const builtInTab = screen.getByRole("tab", { name: "Built-in" });
    // Radix moves roving focus in a macrotask; selection follows focus.
    await waitFor(() => expect(builtInTab).toHaveFocus());
    await waitFor(() => expect(builtInTab).toHaveAttribute("aria-selected", "true"));
    expect(screen.getByRole("heading", { name: "Managed browser", level: 3 })).toBeInTheDocument();

    const user = userEvent.setup();
    await user.click(screen.getByRole("tab", { name: "MCP" }));
    expect(screen.getByRole("heading", { name: "GitHub MCP", level: 3 })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Linear MCP", level: 3 })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Wisp", level: 3 })).not.toBeInTheDocument();
  });

  it("filters out a stale Wisp catalog entry if one appears in the snapshot", async () => {
    const client = new MockDesktopClient();
    const snapshot = await client.getSnapshot();
    snapshot.extensions = [
      ...snapshot.extensions,
      {
        id: "wisp",
        name: "Wisp",
        description: "must not render",
        kind: "managed",
        status: "available",
        permissions: ["Install required"],
        recommended: true,
      },
    ];
    // Override getSnapshot via a thin subclass
    class StaleWispClient extends MockDesktopClient {
      override async getSnapshot() {
        return structuredClone(snapshot);
      }
    }
    renderExtensions(new StaleWispClient());
    expect(await screen.findByRole("heading", { name: "Managed browser", level: 3 })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Wisp", level: 3 })).not.toBeInTheDocument();
    expect(screen.queryByText("Install required")).not.toBeInTheDocument();
  });
});

import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { describe, expect, it } from "vitest";
import { DesktopClientProvider } from "../services/DesktopClientContext";
import { MockDesktopClient } from "../services/mockDesktopClient";
import { App } from "./App";

function renderRoute(route: string, client = new MockDesktopClient()) {
  render(<DesktopClientProvider client={client}><MemoryRouter initialEntries={[route]}><App /></MemoryRouter></DesktopClientProvider>);
  return client;
}

describe("complete product workflows", () => {
  it("starts a Chat conversation and navigates to its completed transcript", async () => {
    renderRoute("/");
    const prompt = await screen.findByLabelText("Message Grok");
    fireEvent.change(prompt, { target: { value: "Review the release plan" } });
    const send = screen.getByRole("button", { name: "Send message" });
    await waitFor(() => expect(send).toBeEnabled());
    fireEvent.click(send);

    await waitFor(() => expect(screen.getByRole("heading", { name: "Review the release plan" })).toBeInTheDocument());
    await waitFor(() => expect(screen.getByText(/organized the response into the key release decisions/)).toBeInTheDocument());
    expect(screen.getByRole("button", { name: "Edit prompt in a new branch" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Regenerate response in a new branch" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Branch from this response" })).toBeEnabled();
  });

  it("keeps subscription unavailable while enrolling BYOK through native entry", async () => {
    renderRoute("/setup", new MockDesktopClient({ firstRun: true }));
    expect(await screen.findByRole("button", { name: "Connection unavailable" })).toBeDisabled();
    expect(screen.getByText(/Subscription status cannot be verified/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /xAI API key/ }));
    fireEvent.click(await screen.findByRole("button", { name: "Add xAI API key" }));
    expect(await screen.findByText("xAI API key stored in the operating system vault.")).toBeInTheDocument();
    expect(screen.getByText("xAI API key configured")).toBeInTheDocument();
    expect(screen.queryByRole("textbox", { name: "xAI API key" })).not.toBeInTheDocument();
  });

  it("removes a configured xAI API key through an explicit vault confirmation", async () => {
    renderRoute("/setup");
    fireEvent.click(await screen.findByRole("button", { name: /xAI API key/ }));
    expect(await screen.findByText("xAI API key configured")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Remove key" }));
    const dialog = screen.getByRole("dialog", { name: "Remove xAI API key?" });
    fireEvent.click(within(dialog).getByRole("button", { name: "Remove key" }));
    expect(await screen.findByText("xAI API key removed from the operating system vault.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Add xAI API key" })).toBeInTheDocument();
  });

  it("opens citations and artifact controls in a full conversation", async () => {
    renderRoute("/conversations/thread-1");
    expect(await screen.findByRole("heading", { name: "Q3 launch narrative" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /Customer interview synthesis/ }));
    expect(await screen.findByRole("heading", { name: "Customer interview synthesis" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Close inspector" }));
    fireEvent.click(screen.getByRole("button", { name: /Launch narrative.md/ }));
    expect(await screen.findByText("# Atlas launch narrative", { exact: false })).toBeInTheDocument();
  });

  it("creates a queued Grok Imagine image with provenance", async () => {
    renderRoute("/library");
    fireEvent.click(screen.getByRole("tab", { name: "Images" }));
    expect(await screen.findByRole("heading", { name: "Create with Grok Imagine" })).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Prompt"), { target: { value: "A clear launch review workspace" } });
    fireEvent.click(screen.getByRole("button", { name: "Create image" }));
    expect(await screen.findByText("A clear launch review workspace")).toBeInTheDocument();
    expect(screen.getAllByText("Created with Grok").length).toBeGreaterThan(0);
  });

  it("opens a realtime Voice session with captions and device controls", async () => {
    renderRoute("/");
    fireEvent.click(screen.getByRole("button", { name: "Start voice input" }));
    expect(await screen.findByRole("heading", { name: "Listening" })).toBeInTheDocument();
    expect(screen.getByLabelText("Microphone")).toBeInTheDocument();
    expect(screen.getByText("Summarize the current launch risks.")).toBeInTheDocument();
  });

  it("creates an automation and opens the recommended Wisp update flow", async () => {
    const client = new MockDesktopClient();
    const { unmount } = render(<DesktopClientProvider client={client}><MemoryRouter initialEntries={["/automations"]}><App /></MemoryRouter></DesktopClientProvider>);
    fireEvent.click(await screen.findByRole("button", { name: "New definition" }));
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Release readiness scan" } });
    fireEvent.change(screen.getByLabelText("Project"), { target: { value: "atlas" } });
    fireEvent.change(screen.getByLabelText("Task prompt"), { target: { value: "Review launch blockers" } });
    fireEvent.change(screen.getByLabelText("Local time"), { target: { value: "09:00" } });
    fireEvent.click(screen.getByRole("button", { name: "Save definition" }));
    expect(await screen.findByText("Definition saved inactive.")).toBeInTheDocument();
    unmount();

    renderRoute("/extensions", client);
    expect(await screen.findByText("Recommended")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "View details" }));
    expect(await screen.findByRole("heading", { name: "Wisp" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /Update to/ }));
    await waitFor(() => expect(screen.getByText("Wisp update completed.")).toBeInTheDocument());
  });
});

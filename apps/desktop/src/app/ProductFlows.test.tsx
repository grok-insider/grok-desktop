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

  it("connects Grok Build host auth and keeps BYOK enrollment secret-free", async () => {
    renderRoute("/setup", new MockDesktopClient({ firstRun: true }));
    const connect = await screen.findByRole("button", { name: "Connect Grok Build" });
    expect(connect).toBeEnabled();
    fireEvent.click(connect);
    expect(await screen.findByText("Grok account connected")).toBeInTheDocument();
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

  it("shows Library files without advertising Imagine media creation", async () => {
    renderRoute("/library");
    expect(await screen.findByRole("tab", { name: "Files" })).toBeInTheDocument();
    expect(screen.queryByRole("tab", { name: "Images" })).not.toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Create with Grok Imagine" })).not.toBeInTheDocument();
    expect(screen.getByText(/Media generation is not available/i)).toBeInTheDocument();
  });

  it("creates an automation definition without advertising live schedule execution", async () => {
    renderRoute("/automations");
    fireEvent.click(await screen.findByRole("button", { name: "New definition" }));
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Release readiness scan" } });
    fireEvent.change(screen.getByLabelText("Project"), { target: { value: "atlas" } });
    fireEvent.change(screen.getByLabelText("Task prompt"), { target: { value: "Review launch blockers" } });
    fireEvent.change(screen.getByLabelText("Local time"), { target: { value: "09:00" } });
    fireEvent.click(screen.getByRole("button", { name: "Save definition" }));
    expect(await screen.findByText(/Definition saved/i)).toBeInTheDocument();
  });
});

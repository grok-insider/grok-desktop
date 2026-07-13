import { useState } from "react";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router-dom";
import { describe, expect, it, vi } from "vitest";
import { DesktopClientProvider } from "../services/DesktopClientContext";
import { MockDesktopClient } from "../services/mockDesktopClient";
import { VoiceOverlay } from "./VoiceOverlay";

function renderVoice(client = new MockDesktopClient(), onClose = vi.fn()) {
  render(
    <DesktopClientProvider client={client}>
      <MemoryRouter>
        <VoiceOverlay onClose={onClose} />
      </MemoryRouter>
    </DesktopClientProvider>,
  );
  return onClose;
}

describe("VoiceOverlay", () => {
  it("traps focus, closes on Escape, and restores focus to its opener", async () => {
    const client = new MockDesktopClient();
    const onClose = vi.fn();

    function Harness() {
      const [open, setOpen] = useState(false);
      return (
        <DesktopClientProvider client={client}>
          <MemoryRouter>
            <button type="button" onClick={() => setOpen(true)}>Open voice</button>
            {open && (
              <VoiceOverlay
                onClose={() => {
                  onClose();
                  setOpen(false);
                }}
              />
            )}
          </MemoryRouter>
        </DesktopClientProvider>
      );
    }

    render(<Harness />);
    const opener = screen.getByRole("button", { name: "Open voice" });
    opener.focus();
    fireEvent.click(opener);

    const close = await screen.findByRole("button", { name: "Close voice" });
    await waitFor(() => expect(close).toHaveFocus());
    await screen.findByRole("heading", { name: "Listening" });

    const end = screen.getByRole("button", { name: "End voice session" });
    end.focus();
    fireEvent.keyDown(end, { key: "Tab" });
    expect(close).toHaveFocus();

    fireEvent.keyDown(close, { key: "Tab", shiftKey: true });
    expect(end).toHaveFocus();

    fireEvent.keyDown(end, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(onClose).toHaveBeenCalledTimes(1);
    await waitFor(() => expect(opener).toHaveFocus());
  });

  it("shows the daemon-provided unavailable reason without starting a session", async () => {
    const client = new MockDesktopClient();
    vi.spyOn(client, "getVoiceSetup").mockResolvedValue({
      capability: "unavailable",
      reason: "Realtime Voice sessions are not exposed by the current daemon protocol.",
      inputDevices: [],
      outputDevices: [],
      selectedInputId: "",
      selectedOutputId: "",
    });
    const start = vi.spyOn(client, "startVoiceSession");

    renderVoice(client);

    expect(await screen.findByRole("heading", { name: "Voice unavailable" })).toBeInTheDocument();
    expect(screen.getByText("Realtime Voice sessions are not exposed by the current daemon protocol.")).toBeInTheDocument();
    expect(screen.queryByLabelText("Microphone")).not.toBeInTheDocument();
    expect(start).not.toHaveBeenCalled();
  });

  it("renders captions and device labels, interrupts, resumes, and ends the session", async () => {
    const client = new MockDesktopClient();
    const start = vi.spyOn(client, "startVoiceSession");
    const setState = vi.spyOn(client, "setVoiceSessionState");
    const onClose = renderVoice(client);

    expect(await screen.findByRole("heading", { name: "Listening" })).toBeInTheDocument();
    expect(start).toHaveBeenCalledWith("default-mic", "default-speaker");
    expect(screen.getByLabelText("Microphone")).toHaveTextContent("Default microphone");
    expect(screen.getByLabelText("Speakers")).toHaveTextContent("Default speakers");
    const user = userEvent.setup();
    await user.click(screen.getByLabelText("Microphone"));
    expect(await screen.findByRole("option", { name: "Studio microphone" })).toBeInTheDocument();
    await user.keyboard("{Escape}");
    await user.click(screen.getByLabelText("Speakers"));
    expect(await screen.findByRole("option", { name: "Headphones" })).toBeInTheDocument();
    await user.keyboard("{Escape}");
    expect(screen.getByLabelText("Live captions")).toHaveTextContent("Summarize the current launch risks.");
    expect(screen.getByText("Streaming")).toHaveClass("sr-only");

    fireEvent.click(screen.getByRole("button", { name: "Interrupt" }));
    expect(await screen.findByRole("heading", { name: "Interrupted" })).toBeInTheDocument();
    expect(setState).toHaveBeenLastCalledWith(expect.any(String), "interrupted");
    expect(screen.getByText("Response interrupted.")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Resume listening" }));
    expect(await screen.findByRole("heading", { name: "Listening" })).toBeInTheDocument();
    expect(setState).toHaveBeenLastCalledWith(expect.any(String), "listening");

    fireEvent.click(screen.getByRole("button", { name: "End voice session" }));
    await waitFor(() => expect(setState).toHaveBeenLastCalledWith(expect.any(String), "ended"));
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});

import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { DesktopClientProvider } from "../services/DesktopClientContext";
import type { ArtifactRemovalResult, DesktopSnapshot, LibraryItem, MediaCreation } from "../services/desktopClient";
import { MockDesktopClient } from "../services/mockDesktopClient";
import { LibraryView } from "./LibraryView";

class MutableLibraryClient extends MockDesktopClient {
  private readonly snapshotListeners = new Set<() => void>();
  private removalPending = false;

  constructor(private readonly mutableSnapshot: DesktopSnapshot) {
    super();
  }

  override async getSnapshot(): Promise<DesktopSnapshot> {
    return structuredClone(this.mutableSnapshot);
  }

  override subscribe(listener: () => void): () => void {
    this.snapshotListeners.add(listener);
    return () => this.snapshotListeners.delete(listener);
  }

  override async removeArtifact(
    artifactId: string,
    _expectedRevision: number,
    _expectedContentVersion: number,
  ): Promise<ArtifactRemovalResult> {
    this.mutableSnapshot.library = this.mutableSnapshot.library.filter((item) => item.id !== artifactId);
    for (const listener of this.snapshotListeners) listener();
    return this.removalPending
      ? { status: "pending" }
      : { status: "success", value: undefined };
  }

  setRemovalPending(): void {
    this.removalPending = true;
  }
}

function renderLibrary(client = new MockDesktopClient()) {
  render(
    <DesktopClientProvider client={client}>
      <LibraryView />
    </DesktopClientProvider>,
  );
  return client;
}

async function productionClient(
  update?: (snapshot: DesktopSnapshot) => void,
): Promise<MockDesktopClient> {
  const client = new MockDesktopClient();
  const snapshot = await client.getSnapshot();
  snapshot.connection = { ...snapshot.connection, interfacePreview: false };
  snapshot.capabilities = snapshot.capabilities.map((capability) => capability.id === "files"
    ? {
        ...capability,
        available: true,
        availability: "available",
        reasonCode: "ready",
        reason: "Available.",
      }
    : capability);
  update?.(snapshot);
  vi.spyOn(client, "getSnapshot").mockResolvedValue(snapshot);
  return client;
}

async function mutableProductionClient(): Promise<MutableLibraryClient> {
  const preview = await new MockDesktopClient().getSnapshot();
  preview.connection = { ...preview.connection, interfacePreview: false };
  preview.capabilities = preview.capabilities.map((capability) => capability.id === "files"
    ? {
        ...capability,
        available: true,
        availability: "available",
        reasonCode: "ready",
        reason: "Available.",
      }
    : capability);
  return new MutableLibraryClient(preview);
}

describe("LibraryView", () => {
  it("reserves the library layout while the daemon snapshot is loading", () => {
    const client = new MockDesktopClient();
    vi.spyOn(client, "getSnapshot").mockImplementation(() => new Promise(() => undefined));

    renderLibrary(client);

    expect(screen.getByRole("status", { name: "Loading library items" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Import file" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Import file" })).toHaveAttribute(
      "title",
      "The Library is still loading.",
    );
  });

  it("shows an honest empty state when the daemon reports no library items", async () => {
    const client = new MockDesktopClient();
    const snapshot = await client.getSnapshot();
    vi.spyOn(client, "getSnapshot").mockResolvedValue({ ...snapshot, library: [] });

    renderLibrary(client);

    expect(await screen.findByRole("heading", { name: "No library items" })).toBeInTheDocument();
    expect(screen.getByText("Files and artifacts will appear after the daemon reports them.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Clear filters" })).not.toBeInTheDocument();
  });

  it("filters and searches daemon-backed metadata, then clears the query and type together", async () => {
    renderLibrary();
    expect(await screen.findByText("Launch narrative.md")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Data" }));
    expect(screen.getByText("Positioning matrix.csv")).toBeInTheDocument();
    expect(screen.queryByText("Launch narrative.md")).not.toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("Search library"), { target: { value: "quarterly" } });
    expect(await screen.findByRole("heading", { name: "No matching items" })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Clear filters" }));
    expect(screen.getByLabelText("Search library")).toHaveValue("");
    expect(screen.getByRole("button", { name: "All" })).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByText("Quarterly trend.png")).toBeInTheDocument();
  });

  it("shows reported metadata in a responsive inspector and restores focus when it closes", async () => {
    renderLibrary();
    const itemButton = await screen.findByRole("button", { name: /Launch narrative\.md/ });

    fireEvent.click(itemButton);

    const inspector = screen.getByRole("complementary", { name: "Launch narrative.md" });
    expect(inspector).not.toHaveClass("max-[900px]:order-first");
    expect(within(inspector).getByText("Atlas launch")).toBeInTheDocument();
    expect(within(inspector).getByText("34 KB")).toBeInTheDocument();
    expect(within(inspector).getByText("v8")).toBeInTheDocument();
    expect(within(inspector).getByText("12 min")).toBeInTheDocument();

    const open = within(inspector).getByRole("button", { name: "Open" });
    const exportButton = within(inspector).getByRole("button", { name: "Export unavailable" });
    const removeButton = within(inspector).getByRole("button", { name: "Remove local copy" });
    expect(open).toBeDisabled();
    expect(open).toHaveAttribute(
      "title",
      "File import, opening, and removal require the installed desktop application.",
    );
    expect(exportButton).toBeDisabled();
    expect(exportButton).toHaveAttribute("title", "File export requires daemon protocol support");
    expect(removeButton).toBeDisabled();
    expect(removeButton).toHaveAttribute(
      "title",
      "File import, opening, and removal require the installed desktop application.",
    );

    fireEvent.click(within(inspector).getByRole("button", { name: "Close preview" }));
    await waitFor(() => expect(screen.queryByRole("complementary")).not.toBeInTheDocument());
    await waitFor(() => expect(itemButton).toHaveFocus());
  });

  it("uses an accessible project dialog, reports native cancellation, and restores focus", async () => {
    const client = await productionClient();
    let settleImport: ((result: { status: "cancelled"; reason: string }) => void) | undefined;
    const importing = new Promise<{ status: "cancelled"; reason: string }>((resolve) => {
      settleImport = resolve;
    });
    const importArtifact = vi.spyOn(client, "importArtifact").mockReturnValue(importing);
    renderLibrary(client);
    const trigger = await screen.findByRole("button", { name: "Import file" });

    fireEvent.click(trigger);
    const dialog = screen.getByRole("dialog", { name: "Import a file" });
    const project = within(dialog).getByLabelText("Project");
    expect(project).toHaveValue("atlas");
    fireEvent.change(project, { target: { value: "ops" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Choose file" }));

    expect(importArtifact).toHaveBeenCalledWith("ops");
    expect(within(dialog).getByRole("status")).toHaveTextContent("Waiting for file selection");
    expect(within(dialog).getByRole("button", { name: "Importing…" })).toBeDisabled();

    await act(async () => {
      settleImport?.({ status: "cancelled", reason: "Import cancelled." });
      await importing;
    });
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Import a file" })).not.toBeInTheDocument());
    expect(screen.getByRole("status")).toHaveTextContent("Import cancelled.");
    await waitFor(() => expect(trigger).toHaveFocus());
  });

  it("keeps import errors in the modal and disables import without an active project", async () => {
    const failing = await productionClient();
    vi.spyOn(failing, "importArtifact").mockRejectedValue(new Error("The file could not be imported."));
    const { unmount } = render(
      <DesktopClientProvider client={failing}><LibraryView /></DesktopClientProvider>,
    );
    const trigger = await screen.findByRole("button", { name: "Import file" });
    fireEvent.click(trigger);
    fireEvent.click(screen.getByRole("button", { name: "Choose file" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("The file could not be imported.");
    expect(screen.getByRole("dialog", { name: "Import a file" })).toBeInTheDocument();

    unmount();
    const withoutProjects = await productionClient((snapshot) => {
      snapshot.projects = [];
    });
    renderLibrary(withoutProjects);
    const disabled = await screen.findByRole("button", { name: "Import file" });
    expect(disabled).toBeDisabled();
    expect(disabled).toHaveAttribute("title", "Create an active project before importing a file.");
  });

  it("opens only an exact content version and announces interrupted review", async () => {
    const client = await productionClient();
    const openArtifact = vi.spyOn(client, "openArtifact").mockResolvedValue({
      status: "success",
      value: {
        artifactId: "asset-1",
        contentVersion: 8,
        status: "interrupted_needs_review",
      },
    });
    renderLibrary(client);
    fireEvent.click(await screen.findByRole("button", { name: /Launch narrative\.md/ }));
    const inspector = screen.getByRole("complementary", { name: "Launch narrative.md" });
    const open = within(inspector).getByRole("button", { name: "Open" });
    expect(open).toBeEnabled();

    fireEvent.click(open);
    await waitFor(() => expect(openArtifact).toHaveBeenCalledWith("asset-1", 8));
    expect(await within(inspector).findByRole("status")).toHaveTextContent(
      "Opening content version 8 was interrupted and needs review. It was not replayed.",
    );
  });

  it("announces closed artifact open failures with a safe recovery path", async () => {
    const cases = [
      [
        "content_unavailable",
        "Content version 8 is missing from local storage. Import the file again to create a verified copy.",
      ],
      [
        "platform_unavailable",
        "The desktop open service is unavailable. Check your desktop session, then try content version 8 again.",
      ],
      [
        "deadline_exceeded",
        "Opening content version 8 timed out before desktop dispatch. Try again.",
      ],
      [
        "integrity_failure",
        "Content version 8 failed its local integrity check and was not opened. Import the file again to create a verified copy.",
      ],
      [
        "interrupted_before_dispatch",
        "Opening content version 8 stopped before desktop dispatch. Try again.",
      ],
    ] as const;

    for (const [failureCode, message] of cases) {
      const client = await productionClient();
      vi.spyOn(client, "openArtifact").mockResolvedValue({
        status: "success",
        value: {
          artifactId: "asset-1",
          contentVersion: 8,
          status: "failed",
          failureCode,
        },
      });
      renderLibrary(client);
      fireEvent.click(await screen.findByRole("button", { name: /Launch narrative\.md/ }));
      const inspector = screen.getByRole("complementary", { name: "Launch narrative.md" });
      fireEvent.click(within(inspector).getByRole("button", { name: "Open" }));

      expect(await within(inspector).findByRole("alert")).toHaveTextContent(message);
      cleanup();
    }
  });

  it("confirms local-copy removal with cancel-first focus and Escape restoration", async () => {
    const client = await productionClient();
    const removeArtifact = vi.spyOn(client, "removeArtifact");
    renderLibrary(client);
    fireEvent.click(await screen.findByRole("button", { name: /Launch narrative\.md/ }));
    const inspector = screen.getByRole("complementary", { name: "Launch narrative.md" });
    const trigger = within(inspector).getByRole("button", { name: "Remove local copy" });

    fireEvent.click(trigger);

    const dialog = screen.getByRole("alertdialog", {
      name: "Remove Launch narrative.md from Grok Desktop?",
    });
    expect(within(dialog).getByText(/Grok Desktop's local imported copy of content version 8 \(34 KB\)/))
      .toBeInTheDocument();
    expect(within(dialog).getByText(/original file you selected remains untouched/i)).toBeInTheDocument();
    expect(within(dialog).getByText(/files already opened in another app may remain available there/i))
      .toBeInTheDocument();
    const cancel = within(dialog).getByRole("button", { name: "Cancel" });
    await waitFor(() => expect(cancel).toHaveFocus());

    fireEvent.keyDown(document, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument());
    await waitFor(() => expect(trigger).toHaveFocus());
    expect(removeArtifact).not.toHaveBeenCalled();
  });

  it("serializes removal submission and keeps a safe error inside the confirmation", async () => {
    const client = await productionClient();
    let settleRemoval: ((result: { status: "unavailable"; reason: string }) => void) | undefined;
    const removing = new Promise<{ status: "unavailable"; reason: string }>((resolve) => {
      settleRemoval = resolve;
    });
    const removeArtifact = vi.spyOn(client, "removeArtifact").mockReturnValue(removing);
    renderLibrary(client);
    fireEvent.click(await screen.findByRole("button", { name: /Launch narrative\.md/ }));
    fireEvent.click(screen.getByRole("button", { name: "Remove local copy" }));
    const dialog = screen.getByRole("alertdialog");
    const confirm = within(dialog).getByRole("button", { name: "Remove local copy" });

    fireEvent.click(confirm);
    fireEvent.click(confirm);

    expect(removeArtifact).toHaveBeenCalledOnce();
    expect(removeArtifact).toHaveBeenCalledWith("asset-1", 8, 8);
    expect(within(dialog).getByRole("status")).toHaveTextContent("Removing local copy");
    expect(within(dialog).getByRole("button", { name: "Removing…" })).toBeDisabled();
    expect(within(dialog).getByRole("button", { name: "Cancel" })).toBeDisabled();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.getByRole("alertdialog")).toBeInTheDocument();

    await act(async () => {
      settleRemoval?.({ status: "unavailable", reason: "The artifact changed before removal." });
      await removing;
    });
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(
      "The artifact changed before removal.",
    );
    expect(screen.getByRole("alertdialog")).toBeInTheDocument();
  });

  it("removes the canonical card, announces success, and focuses the next item", async () => {
    const client = await mutableProductionClient();
    const removeArtifact = vi.spyOn(client, "removeArtifact");
    renderLibrary(client);
    const removedCard = await screen.findByRole("button", { name: /Launch narrative\.md/ });
    fireEvent.click(removedCard);
    fireEvent.click(screen.getByRole("button", { name: "Remove local copy" }));
    const dialog = screen.getByRole("alertdialog");
    fireEvent.click(within(dialog).getByRole("button", { name: "Remove local copy" }));

    await waitFor(() => expect(removeArtifact).toHaveBeenCalledWith("asset-1", 8, 8));
    await waitFor(() => expect(screen.queryByText("Launch narrative.md")).not.toBeInTheDocument());
    expect(screen.getByRole("status")).toHaveTextContent(
      "Launch narrative.md was removed from Grok Desktop. The original file remains untouched.",
    );
    await waitFor(() => expect(
      screen.getByRole("button", { name: /Positioning matrix\.csv/ }),
    ).toHaveFocus());
  });

  it("announces daemon-owned private cleanup without claiming terminal completion", async () => {
    const client = await mutableProductionClient();
    client.setRemovalPending();
    const removeArtifact = vi.spyOn(client, "removeArtifact");
    renderLibrary(client);
    fireEvent.click(await screen.findByRole("button", { name: /Launch narrative\.md/ }));
    fireEvent.click(screen.getByRole("button", { name: "Remove local copy" }));
    fireEvent.click(within(screen.getByRole("alertdialog")).getByRole("button", {
      name: "Remove local copy",
    }));

    await waitFor(() => expect(removeArtifact).toHaveBeenCalledWith("asset-1", 8, 8));
    await waitFor(() => expect(screen.queryByText("Launch narrative.md")).not.toBeInTheDocument());
    expect(screen.getByRole("status")).toHaveTextContent(
      "Launch narrative.md was removed from Library; private cleanup is still continuing. The original file remains untouched.",
    );
    expect(screen.getByRole("status")).not.toHaveTextContent(/storage freed|bytes erased|cleanup complete/i);
  });

  it("never enables Open for a legacy item without a content version", async () => {
    const legacy: LibraryItem = {
      id: "legacy-artifact",
      name: "Legacy notes.txt",
      type: "document",
      projectName: "Atlas launch",
      updatedAt: "Now",
      size: "Size unavailable",
      metadataRevision: 4,
    };
    const client = await productionClient((snapshot) => {
      snapshot.library = [legacy];
    });
    const openArtifact = vi.spyOn(client, "openArtifact");
    renderLibrary(client);
    fireEvent.click(await screen.findByRole("button", { name: /Legacy notes\.txt/ }));
    const open = within(
      screen.getByRole("complementary", { name: "Legacy notes.txt" }),
    ).getByRole("button", { name: "Open" });
    const remove = within(
      screen.getByRole("complementary", { name: "Legacy notes.txt" }),
    ).getByRole("button", { name: "Remove local copy" });

    expect(open).toBeDisabled();
    expect(open).toHaveAttribute("title", "This item has no immutable content version to open.");
    expect(openArtifact).not.toHaveBeenCalled();
    expect(remove).toBeDisabled();
    expect(remove).toHaveAttribute("title", "This item has no immutable content version to remove.");
  });

  it("exposes only the Files library surface without Imagine media tabs", async () => {
    renderLibrary();
    const filesTab = screen.getByRole("tab", { name: "Files" });
    const filesPanel = document.getElementById("library-files-panel");

    expect(filesTab).toHaveAttribute("aria-controls", "library-files-panel");
    expect(filesTab).toHaveAttribute("aria-selected", "true");
    expect(filesPanel).not.toHaveAttribute("hidden");
    expect(screen.queryByRole("tab", { name: "Images" })).not.toBeInTheDocument();
    expect(screen.queryByRole("tab", { name: "Videos" })).not.toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Create with Grok Imagine" })).not.toBeInTheDocument();
    expect(screen.getByText(/Media generation is not available/i)).toBeInTheDocument();
  });
});

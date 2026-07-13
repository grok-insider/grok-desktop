import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";
import { describe, expect, it, vi } from "vitest";
import { DesktopClientProvider } from "../services/DesktopClientContext";
import type { ClientResult, CreateProjectInput, DesktopSnapshot, ProjectSummary } from "../services/desktopClient";
import { initialSnapshot } from "../services/mockData";
import { MockDesktopClient } from "../services/mockDesktopClient";
import { ProjectsView } from "./ProjectsView";

class SnapshotClient extends MockDesktopClient {
  constructor(private readonly value: DesktopSnapshot) {
    super();
  }

  override async getSnapshot(): Promise<DesktopSnapshot> {
    return structuredClone(this.value);
  }
}

class PendingSnapshotClient extends MockDesktopClient {
  override async getSnapshot(): Promise<DesktopSnapshot> {
    return new Promise<DesktopSnapshot>(() => undefined);
  }
}

class RejectingProjectClient extends MockDesktopClient {
  readonly createCalls: CreateProjectInput[] = [];

  override async createProject(input: CreateProjectInput): Promise<ClientResult<ProjectSummary>> {
    this.createCalls.push(input);
    return { status: "unavailable", reason: "Project storage is temporarily unavailable." };
  }
}

function LocationProbe() {
  const location = useLocation();
  return <output data-testid="location">{location.pathname}</output>;
}

function renderProjects(initialEntry = "/projects/atlas", client: MockDesktopClient = new MockDesktopClient()) {
  render(
    <DesktopClientProvider client={client}>
      <MemoryRouter initialEntries={[initialEntry]}>
        <LocationProbe />
        <Routes>
          <Route path="/projects" element={<ProjectsView />} />
          <Route path="/projects/:projectId" element={<ProjectsView />} />
          <Route path="/" element={<h1>Home</h1>} />
          <Route path="*" element={<h1>Unexpected route</h1>} />
        </Routes>
      </MemoryRouter>
    </DesktopClientProvider>,
  );
  return client;
}

describe("ProjectsView", () => {
  it("reserves both project columns while the daemon snapshot is loading", () => {
    renderProjects("/projects", new PendingSnapshotClient());

    expect(screen.getByRole("status", { name: "Loading projects" })).toBeInTheDocument();
    expect(screen.getByLabelText("Loading project details")).toBeInTheDocument();
    expect(screen.getByTestId("projects-layout")).toHaveAttribute("aria-busy", "true");
    expect(screen.getByTestId("location")).toHaveTextContent("/projects");
  });

  it("keeps an empty snapshot at its valid route instead of navigating to an undefined project", async () => {
    const emptySnapshot = structuredClone(initialSnapshot);
    emptySnapshot.projects = [];
    renderProjects("/projects", new SnapshotClient(emptySnapshot));

    expect(await screen.findByRole("heading", { name: "No projects yet" })).toBeInTheDocument();
    expect(screen.getByRole("navigation", { name: "Projects" })).toBeInTheDocument();
    expect(screen.getByTestId("location")).toHaveTextContent("/projects");
    expect(screen.getByTestId("location")).not.toHaveTextContent("undefined");
  });

  it("canonicalizes an unknown project ID before selecting the first real project", async () => {
    renderProjects("/projects/not-a-project");

    await waitFor(() => expect(screen.getByTestId("location")).toHaveTextContent("/projects/atlas"));
    expect(await screen.findByRole("heading", { name: "Atlas launch" })).toBeInTheDocument();
    const atlas = within(screen.getByRole("navigation", { name: "Projects" })).getByRole("button", {
      name: /Atlas launch/,
    });
    expect(atlas).toHaveAttribute("aria-current", "page");
  });

  it("selects a daemon-backed project through the project navigator", async () => {
    renderProjects();
    expect(await screen.findByRole("heading", { name: "Atlas launch" })).toBeInTheDocument();

    const navigator = screen.getByRole("navigation", { name: "Projects" });
    const operations = within(navigator).getByRole("button", { name: /Operations/ });
    fireEvent.click(operations);

    expect(await screen.findByRole("heading", { name: "Operations" })).toBeInTheDocument();
    expect(operations).toHaveAttribute("aria-current", "page");
    expect(screen.getByTestId("location")).toHaveTextContent("/projects/ops");
  });

  it("supports roving keyboard focus and labelled responsive project tabs", async () => {
    renderProjects();
    await screen.findByRole("heading", { name: "Atlas launch" });

    const tablist = screen.getByRole("tablist", { name: "Project sections" });
    expect(tablist).toHaveAttribute("aria-orientation", "horizontal");
    const overview = within(tablist).getByRole("tab", { name: "Overview" });
    overview.focus();
    fireEvent.keyDown(overview, { key: "ArrowRight" });

    const conversations = within(tablist).getByRole("tab", { name: "Conversations" });
    // Radix moves roving focus in a macrotask; selection follows focus.
    await waitFor(() => expect(conversations).toHaveFocus());
    await waitFor(() => expect(conversations).toHaveAttribute("aria-selected", "true"));
    expect(screen.getByRole("tabpanel", { name: "Conversations" })).toBeInTheDocument();
    expect(screen.getByText("Q3 launch narrative")).toBeInTheDocument();

    fireEvent.keyDown(conversations, { key: "End" });
    const instructions = within(tablist).getByRole("tab", { name: "Instructions" });
    await waitFor(() => expect(instructions).toHaveFocus());
    await waitFor(() => expect(screen.getByRole("tabpanel", { name: "Instructions" })).toBeInTheDocument());
    expect(screen.getByLabelText("Project instructions")).toHaveValue(
      "Prioritize evidence-backed recommendations and flag assumptions.",
    );
  });

  it("creates a project through the DesktopClient and navigates to its canonical route", async () => {
    const client = new MockDesktopClient();
    const createProject = vi.spyOn(client, "createProject");
    renderProjects("/projects/atlas", client);
    await screen.findByRole("heading", { name: "Atlas launch" });

    fireEvent.click(screen.getByRole("button", { name: "New project" }));
    const dialog = await screen.findByRole("dialog", { name: "Create project" });
    const name = within(dialog).getByLabelText("Project name");
    expect(name).toHaveFocus();
    fireEvent.change(name, { target: { value: "  Release readiness  " } });
    fireEvent.change(within(dialog).getByLabelText("Description"), {
      target: { value: "  Decisions and launch checks  " },
    });
    fireEvent.click(within(dialog).getByRole("button", { name: "Create project" }));

    await waitFor(() => expect(createProject).toHaveBeenCalledWith({
      name: "Release readiness",
      description: "Decisions and launch checks",
    }));
    expect(await screen.findByRole("heading", { name: "Release readiness" })).toBeInTheDocument();
    expect(screen.queryByRole("dialog", { name: "Create project" })).not.toBeInTheDocument();
    expect(screen.getByTestId("location").textContent).toMatch(/^\/projects\/project-/);
  });

  it("keeps a rejected creation usable and restores focus after dismissal", async () => {
    const client = new RejectingProjectClient();
    renderProjects("/projects/atlas", client);
    await screen.findByRole("heading", { name: "Atlas launch" });
    const trigger = screen.getByRole("button", { name: "New project" });
    trigger.focus();
    fireEvent.click(trigger);

    const dialog = await screen.findByRole("dialog", { name: "Create project" });
    const name = within(dialog).getByLabelText("Project name");
    fireEvent.change(name, { target: { value: "Retryable project" } });
    fireEvent.click(within(dialog).getByRole("button", { name: "Create project" }));

    expect(await within(dialog).findByRole("alert")).toHaveTextContent("Project storage is temporarily unavailable.");
    expect(name).toHaveValue("Retryable project");
    await waitFor(() => expect(within(dialog).getByRole("button", { name: "Create project" })).toBeEnabled());
    expect(client.createCalls).toEqual([{ name: "Retryable project", description: "" }]);

    fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Create project" })).not.toBeInTheDocument());
    await waitFor(() => expect(trigger).toHaveFocus());
  });

  it("closes the creation dialog with Escape and returns keyboard focus to its trigger", async () => {
    renderProjects();
    await screen.findByRole("heading", { name: "Atlas launch" });
    const trigger = screen.getByRole("button", { name: "New project" });
    trigger.focus();
    fireEvent.click(trigger);
    expect(await screen.findByLabelText("Project name")).toHaveFocus();

    fireEvent.keyDown(document, { key: "Escape" });

    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Create project" })).not.toBeInTheDocument());
    await waitFor(() => expect(trigger).toHaveFocus());
  });
});

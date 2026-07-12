import { render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { describe, expect, it } from "vitest";
import { DesktopClientProvider } from "../services/DesktopClientContext";
import { MockDesktopClient } from "../services/mockDesktopClient";
import { HomeView } from "./HomeView";

describe("HomeView", () => {
  it("counts saved automation definitions instead of implying they are enabled", async () => {
    render(
      <DesktopClientProvider client={new MockDesktopClient()}>
        <MemoryRouter>
          <HomeView />
        </MemoryRouter>
      </DesktopClientProvider>,
    );

    const metric = (await screen.findByText("saved definitions")).closest("span");
    expect(metric).not.toBeNull();
    expect(metric).toHaveTextContent("3 saved definitions");
    expect(screen.queryByText(/^automations$/)).not.toBeInTheDocument();
  });
});

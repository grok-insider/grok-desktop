import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { DesktopBridgeUnavailable } from "./DesktopBridgeUnavailable";

describe("DesktopBridgeUnavailable", () => {
  it("renders a fatal, accessible bridge error", () => {
    render(<DesktopBridgeUnavailable />);
    expect(screen.getByRole("alert")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Desktop bridge unavailable" })).toBeInTheDocument();
    expect(screen.getByText(/isolated Electron bridge/)).toBeInTheDocument();
  });
});

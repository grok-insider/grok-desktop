import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { MarkdownMessage } from "./MarkdownMessage";

describe("MarkdownMessage", () => {
  it("renders structured markdown while dropping raw HTML", () => {
    render(
      <MarkdownMessage onOpenExternal={() => undefined}>
        {"## Grok Desktop\n\n**Ready**\n\n<script>unsafe()</script>"}
      </MarkdownMessage>,
    );
    expect(screen.getByRole("heading", { name: "Grok Desktop" })).toBeInTheDocument();
    expect(screen.getByText("Ready").tagName).toBe("STRONG");
    expect(screen.queryByText("unsafe()")).not.toBeInTheDocument();
  });

  it("brokers only HTTPS links through Electron", () => {
    const onOpenExternal = vi.fn();
    render(
      <MarkdownMessage onOpenExternal={onOpenExternal}>
        {"[Official](https://docs.x.ai/docs) [Local](file:///tmp/private)"}
      </MarkdownMessage>,
    );
    fireEvent.click(screen.getByRole("link", { name: "Official" }));
    expect(onOpenExternal).toHaveBeenCalledWith("https://docs.x.ai/docs");
    expect(screen.queryByRole("link", { name: "Local" })).not.toBeInTheDocument();
    expect(screen.getByText("Local")).toBeInTheDocument();
  });
});

// @vitest-environment node
import { describe, expect, it } from "vitest";
import { parseArtifactImportDialogResult } from "./artifactImportSelection.js";

describe("parseArtifactImportDialogResult", () => {
  it("distinguishes native cancellation from a selected file", () => {
    expect(parseArtifactImportDialogResult({ canceled: true, filePaths: [] }, "linux"))
      .toEqual({ kind: "cancelled" });
    expect(parseArtifactImportDialogResult({
      canceled: false,
      filePaths: ["/home/person/Quarterly report.pdf"],
    }, "linux")).toEqual({
      kind: "selected",
      sourcePath: "/home/person/Quarterly report.pdf",
      displayName: "Quarterly report.pdf",
      mediaType: "application/pdf",
    });
    expect(parseArtifactImportDialogResult({
      canceled: false,
      filePaths: [String.raw`C:\Users\person\notes.unknown`],
    }, "win32")).toMatchObject({
      displayName: "notes.unknown",
      mediaType: "application/octet-stream",
    });
  });

  it("rejects missing, multiple, relative, and non-portable native results", () => {
    const invalid = [
      { canceled: false, filePaths: [] },
      { canceled: false, filePaths: ["/tmp/one.txt", "/tmp/two.txt"] },
      { canceled: false, filePaths: ["relative/private.txt"] },
      { canceled: true, filePaths: ["/tmp/inconsistent.txt"] },
      { canceled: false, filePaths: ["/tmp/legacy:name.txt"] },
      { canceled: false, filePaths: ["/tmp/invalid-\ud800.txt"] },
      { canceled: false, filePaths: ["/tmp/CON"] },
      { canceled: false, filePaths: ["/tmp/CLOCK$.txt"] },
      { canceled: false, filePaths: ["/tmp/CON .txt"] },
      { canceled: false, filePaths: ["/tmp/LPT1 .pdf"] },
      { canceled: false, filePaths: [`/${"p".repeat(32 * 1024)}`] },
    ];
    for (const result of invalid) {
      expect(() => parseArtifactImportDialogResult(result, "linux"))
        .toThrow("native artifact selection is invalid");
    }
  });

  it("never includes the selected path in validation errors", () => {
    const canary = "relative/secret-source-canary.txt";
    try {
      parseArtifactImportDialogResult({ canceled: false, filePaths: [canary] }, "linux");
      throw new Error("expected selection rejection");
    } catch (error) {
      expect(String(error)).not.toContain("secret-source-canary");
    }
  });
});

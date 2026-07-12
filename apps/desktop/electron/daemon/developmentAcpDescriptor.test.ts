// @vitest-environment node
import { describe, expect, it } from "vitest";
import {
  applyDevelopmentAcpDescriptor,
  resolveDevelopmentAcpDescriptor,
  validDevelopmentExecutable,
  validDevelopmentSha256,
  validDevelopmentVersion,
} from "./developmentAcpDescriptor.js";

describe("developmentAcpDescriptor", () => {
  it("forwards a complete explicit development triple", () => {
    const descriptor = resolveDevelopmentAcpDescriptor({
      platform: "linux",
      env: {
        GROK_ACP_EXECUTABLE: "/opt/xai/bin/grok",
        GROK_ACP_VERSION: "0.2.97",
        GROK_ACP_SHA256: "A".repeat(64),
      },
      resolveRealPath: (filePath) => filePath,
      hashFile: () => {
        throw new Error("hash should not run when sha is provided");
      },
      readVersion: () => {
        throw new Error("version should not run when version is provided");
      },
      findOnPath: () => {
        throw new Error("path lookup should not run when executable is provided");
      },
    });

    expect(descriptor).toEqual({
      executable: "/opt/xai/bin/grok",
      version: "0.2.97",
      sha256: "a".repeat(64),
    });
  });

  it("auto-detects the official grok command from PATH", () => {
    const descriptor = resolveDevelopmentAcpDescriptor({
      platform: "linux",
      env: { PATH: "/usr/bin:/opt/xai/bin" },
      findOnPath: (name) => (name === "grok" ? "/opt/xai/bin/grok" : undefined),
      resolveRealPath: (filePath) => filePath,
      hashFile: (filePath) => (filePath === "/opt/xai/bin/grok" ? "b".repeat(64) : undefined),
      readVersion: (executable) =>
        executable === "/opt/xai/bin/grok" ? "grok 0.2.97 (e6e4fe4262) [alpha]\n" : undefined,
    });

    expect(descriptor).toEqual({
      executable: "/opt/xai/bin/grok",
      version: "0.2.97",
      sha256: "b".repeat(64),
    });
  });

  it("rejects paths that no longer end with the official grok basename after realpath", () => {
    const descriptor = resolveDevelopmentAcpDescriptor({
      platform: "linux",
      env: { PATH: "/opt/xai/bin" },
      findOnPath: () => "/opt/xai/bin/grok",
      resolveRealPath: () => "/opt/xai/libexec/grok-launcher",
      hashFile: () => "c".repeat(64),
      readVersion: () => "0.2.97",
    });

    expect(descriptor).toBeUndefined();
  });

  it("rejects incomplete or workspace-root overrides", () => {
    expect(
      resolveDevelopmentAcpDescriptor({
        platform: "linux",
        env: {
          GROK_ACP_EXECUTABLE: "/opt/xai/bin/grok",
          GROK_ACP_VERSION: "0.2.97",
          GROK_ACP_SHA256: "d".repeat(64),
          GROK_ACP_WORKSPACE_ROOTS: "/tmp/workspace",
        },
        resolveRealPath: (filePath) => filePath,
      }),
    ).toBeUndefined();

    expect(
      resolveDevelopmentAcpDescriptor({
        platform: "linux",
        env: { GROK_ACP_EXECUTABLE: "/opt/xai/bin/grok" },
        resolveRealPath: (filePath) => filePath,
        hashFile: () => undefined,
        readVersion: () => undefined,
      }),
    ).toBeUndefined();
  });

  it("applies the descriptor onto a child environment", () => {
    const environment: NodeJS.ProcessEnv = { PATH: "/safe/bin" };
    applyDevelopmentAcpDescriptor(environment, {
      executable: "/opt/xai/bin/grok",
      version: "0.2.97",
      sha256: "e".repeat(64),
    });
    expect(environment).toMatchObject({
      PATH: "/safe/bin",
      GROK_ACP_EXECUTABLE: "/opt/xai/bin/grok",
      GROK_ACP_VERSION: "0.2.97",
      GROK_ACP_SHA256: "e".repeat(64),
    });
  });

  it("validates development override shapes", () => {
    expect(validDevelopmentExecutable("/opt/xai/bin/grok", "linux")).toBe("/opt/xai/bin/grok");
    expect(validDevelopmentExecutable("grok", "linux")).toBeUndefined();
    expect(validDevelopmentExecutable("/opt/xai/bin/grok\n", "linux")).toBeUndefined();
    expect(validDevelopmentVersion("0.2.97")).toBe("0.2.97");
    expect(validDevelopmentVersion("not-a-version")).toBeUndefined();
    expect(validDevelopmentSha256("f".repeat(64))).toBe("f".repeat(64));
    expect(validDevelopmentSha256("zz")).toBeUndefined();
  });

});

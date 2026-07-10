// @vitest-environment node
import { describe, expect, it } from "vitest";
import { isTrustedTopLevelAppSender } from "./trustedSenderPolicy.js";

const applicationDocument = "grok-desktop://app/index.html";

describe("isTrustedTopLevelAppSender", () => {
  it("accepts only the primary window's top-level application document", () => {
    expect(isTrustedTopLevelAppSender({
      ownsPrimaryWindow: true,
      hasSenderFrame: true,
      isTopLevelFrame: true,
      frameUrl: `${applicationDocument}#/conversations/thread-1`,
    }, applicationDocument)).toBe(true);

    for (const sender of [
      { ownsPrimaryWindow: false, hasSenderFrame: true, isTopLevelFrame: true, frameUrl: applicationDocument },
      { ownsPrimaryWindow: true, hasSenderFrame: false, isTopLevelFrame: true, frameUrl: applicationDocument },
      { ownsPrimaryWindow: true, hasSenderFrame: true, isTopLevelFrame: false, frameUrl: applicationDocument },
      { ownsPrimaryWindow: true, hasSenderFrame: true, isTopLevelFrame: true, frameUrl: "https://example.com/" },
      { ownsPrimaryWindow: true, hasSenderFrame: true, isTopLevelFrame: true, frameUrl: "grok-desktop://open/v1/home" },
    ]) {
      expect(isTrustedTopLevelAppSender(sender, applicationDocument)).toBe(false);
    }
  });
});

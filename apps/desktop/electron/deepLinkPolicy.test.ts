// @vitest-environment node
import { describe, expect, it } from "vitest";
import {
  DESKTOP_DEEP_LINK_VERSION,
  hasDesktopDeepLinkArgument,
  parseDesktopDeepLink,
  parseDesktopDeepLinkFromArgv,
  rendererHashForDesktopDeepLink,
} from "./deepLinkPolicy.js";

describe("desktop deep-link v1 policy", () => {
  it.each([
    ["home", "#/"],
    ["projects", "#/projects"],
    ["activity", "#/activity"],
    ["library", "#/library"],
    ["automations", "#/automations"],
    ["extensions", "#/extensions"],
    ["settings", "#/settings"],
  ] as const)("accepts the exact safe top-level %s route", (route, rendererHash) => {
    const parsed = parseDesktopDeepLink(`grok-desktop://open/v1/${route}`);
    expect(parsed).toEqual({ version: DESKTOP_DEEP_LINK_VERSION, route });
    expect(parsed && rendererHashForDesktopDeepLink(parsed)).toBe(rendererHash);
    expect(Object.isFrozen(parsed)).toBe(true);
  });

  it("preserves exact bounded project and conversation identifiers", () => {
    const projectId = `project-${"a".repeat(120)}`;
    const threadId = `thread-${"Z_9-".repeat(30)}Z`;

    const project = parseDesktopDeepLink(`grok-desktop://open/v1/projects/${projectId}`);
    const conversation = parseDesktopDeepLink(`grok-desktop://open/v1/conversations/${threadId}`);

    expect(project).toEqual({ version: 1, route: "project", projectId });
    expect(conversation).toEqual({ version: 1, route: "conversation", threadId });
    expect(project && rendererHashForDesktopDeepLink(project)).toBe(`#/projects/${projectId}`);
    expect(conversation && rendererHashForDesktopDeepLink(conversation)).toBe(`#/conversations/${threadId}`);
  });

  it("selects one valid link from packaged and development argv shapes", () => {
    expect(parseDesktopDeepLinkFromArgv([
      "C:\\Program Files\\Grok Desktop\\Grok Desktop.exe",
      "grok-desktop://open/v1/settings",
    ])).toEqual({ version: 1, route: "settings" });
    expect(parseDesktopDeepLinkFromArgv([
      "/usr/bin/electron",
      ".",
      "--inspect=0",
      "grok-desktop://open/v1/conversations/thread-1",
    ])).toEqual({ version: 1, route: "conversation", threadId: "thread-1" });
  });

  it("rejects ambiguous argv with multiple valid links", () => {
    expect(parseDesktopDeepLinkFromArgv([
      "grok-desktop://open/v1/home",
      "grok-desktop://open/v1/settings",
    ])).toBeNull();
    expect(parseDesktopDeepLinkFromArgv([
      "grok-desktop://open/v1/home",
      "grok-desktop://open/v1/home",
    ])).toBeNull();
  });

  it("distinguishes ordinary second launches from malformed scheme activation", () => {
    expect(hasDesktopDeepLinkArgument(["Grok Desktop.exe", "--profile=default"])).toBe(false);
    expect(hasDesktopDeepLinkArgument(["Grok Desktop.exe", "grok-desktop://open/v2/home"])).toBe(true);
    expect(hasDesktopDeepLinkArgument(["GROK-DESKTOP://app/index.html"])).toBe(true);
  });

  it.each([
    undefined,
    null,
    1,
    true,
    {},
    [],
    new URL("grok-desktop://open/v1/home"),
    "",
  ])("rejects non-string or empty input: %o", (value) => {
    expect(parseDesktopDeepLink(value)).toBeNull();
  });

  it.each([
    "/projects/project-1",
    "#/projects/project-1",
    "projects/project-1",
    "https://open/v1/home",
    "http://open/v1/home",
    "file:///v1/home",
    "javascript:alert(1)",
    "data:text/plain,home",
    "shell:open",
    "powershell:Start-Process",
    "grok-desktop:v1/home",
    "grok-desktop:/open/v1/home",
    "grok-desktop:////open/v1/home",
  ])("rejects raw paths and foreign or opaque URL schemes: %s", (value) => {
    expect(parseDesktopDeepLink(value)).toBeNull();
  });

  it.each([
    "grok-desktop://app/index.html",
    "grok-desktop://app/index.html#/settings",
    "grok-desktop://app/v1/home",
    "grok-desktop://other/v1/home",
    "grok-desktop://open.example/v1/home",
    "grok-desktop://open./v1/home",
    "grok-desktop://.open/v1/home",
    "grok-desktop://OPEN/v1/home",
    "GROK-DESKTOP://open/v1/home",
  ])("rejects the internal application origin and non-canonical authorities: %s", (value) => {
    expect(parseDesktopDeepLink(value)).toBeNull();
  });

  it.each([
    "grok-desktop://user@open/v1/home",
    "grok-desktop://user:password@open/v1/home",
    "grok-desktop://@open/v1/home",
    "grok-desktop://open:0/v1/home",
    "grok-desktop://open:443/v1/home",
    "grok-desktop://open:/v1/home",
  ])("rejects userinfo and ports, including empty authority components: %s", (value) => {
    expect(parseDesktopDeepLink(value)).toBeNull();
  });

  it.each([
    "grok-desktop://open/v1/home?prompt=ignore",
    "grok-desktop://open/v1/home?url=https://example.com",
    "grok-desktop://open/v1/home?command=run",
    "grok-desktop://open/v1/home?",
    "grok-desktop://open/v1/home#settings",
    "grok-desktop://open/v1/home#",
    "grok-desktop://open/v1/projects/project-1?x=1#y",
  ])("rejects every query and fragment, including empty markers: %s", (value) => {
    expect(parseDesktopDeepLink(value)).toBeNull();
  });

  it.each([
    "\0grok-desktop://open/v1/home",
    "grok-desktop://open/v1/ho\u0000me",
    "grok-desktop://open/v1/home\n",
    "grok-desktop://open/v1/\thome",
    "grok-desktop://open/v1/home\r",
    "grok-desktop://open/v1/home\u007f",
    "grok-desktop://open/v1/home\u0085",
  ])("rejects control characters before URL normalization: %o", (value) => {
    expect(parseDesktopDeepLink(value)).toBeNull();
  });

  it.each([
    "grok-desktop://open/v1/%68ome",
    "grok-desktop://open/v1/projects/project%2Fone",
    "grok-desktop://open/v1/projects/project%2fone",
    "grok-desktop://open/v1/projects/project%5Cone",
    "grok-desktop://open/v1/projects/project%5cone",
    "grok-desktop://open/v1/%2e%2e/settings",
    "grok-desktop://open/v1/%2E%2E/settings",
    "grok-desktop://open/v1/%252e%252e/settings",
    "grok-desktop://open/v1/projects/project-%252fsecret",
    "grok-desktop://open/v1/projects/project-1\\..\\settings",
    "grok-desktop://open/v1/./home",
    "grok-desktop://open/v1/projects/../settings",
    "grok-desktop://open/v1//home",
  ])("rejects encoded or normalized separators and traversal: %s", (value) => {
    expect(parseDesktopDeepLink(value)).toBeNull();
  });

  it.each([
    "grok-desktop://open/",
    "grok-desktop://open/v1",
    "grok-desktop://open/v1/",
    "grok-desktop://open/v0/home",
    "grok-desktop://open/v2/home",
    "grok-desktop://open/V1/home",
    "grok-desktop://open/v1/Home",
    "grok-desktop://open/v1/setup",
    "grok-desktop://open/v1/voice",
    "grok-desktop://open/v1/unknown",
    "grok-desktop://open/v1/home/",
    "grok-desktop://open/v1/home/extra",
    "grok-desktop://open/v1/project/project-1",
    "grok-desktop://open/v1/conversation/thread-1",
  ])("rejects unknown versions, routes, setup, and non-canonical path shapes: %s", (value) => {
    expect(parseDesktopDeepLink(value)).toBeNull();
  });

  it.each([
    "grok-desktop://open/v1/projects/",
    "grok-desktop://open/v1/projects/thread-1",
    "grok-desktop://open/v1/projects/project-",
    "grok-desktop://open/v1/projects/project 1",
    "grok-desktop://open/v1/projects/project.+1",
    "grok-desktop://open/v1/projects/project-ä",
    "grok-desktop://open/v1/projects/project-1/extra",
    "grok-desktop://open/v1/conversations",
    "grok-desktop://open/v1/conversations/",
    "grok-desktop://open/v1/conversations/project-1",
    "grok-desktop://open/v1/conversations/thread-",
    "grok-desktop://open/v1/conversations/thread:1",
    "grok-desktop://open/v1/conversations/thread@1",
    "grok-desktop://open/v1/conversations/thread-1/extra",
  ])("rejects absent, cross-kind, unsafe, and compound entity identifiers: %s", (value) => {
    expect(parseDesktopDeepLink(value)).toBeNull();
  });

  it("rejects entity identifiers and whole inputs beyond their byte bounds", () => {
    const overlongProjectId = `project-${"a".repeat(121)}`;
    const overlongThreadId = `thread-${"a".repeat(122)}`;
    expect(overlongProjectId).toHaveLength(129);
    expect(overlongThreadId).toHaveLength(129);
    expect(parseDesktopDeepLink(`grok-desktop://open/v1/projects/${overlongProjectId}`)).toBeNull();
    expect(parseDesktopDeepLink(`grok-desktop://open/v1/conversations/${overlongThreadId}`)).toBeNull();
    expect(parseDesktopDeepLink(`grok-desktop://open/v1/${"a".repeat(257)}`)).toBeNull();
  });

  it.each([
    "grok-desktop://open/v1/prompt/hello",
    "grok-desktop://open/v1/chat/tell-me-a-secret",
    "grok-desktop://open/v1/commands/run",
    "grok-desktop://open/v1/run/powershell",
    "grok-desktop://open/v1/open/https://example.com",
    "grok-desktop://open/v1/files/C:/Windows/System32",
    "grok-desktop://open/v1/files//etc/passwd",
    "grok-desktop://open/v1/import/home/friend/file.txt",
  ])("rejects prompt, command, external-URL, and file-path payload routes: %s", (value) => {
    expect(parseDesktopDeepLink(value)).toBeNull();
  });
});

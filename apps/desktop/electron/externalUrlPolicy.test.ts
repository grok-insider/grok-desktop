// @vitest-environment node
import { describe, expect, it } from "vitest";
import { parseExternalHttpsUrl } from "./externalUrlPolicy.js";

describe("parseExternalHttpsUrl", () => {
  it("accepts exact canonical public HTTPS URLs", () => {
    for (const url of [
      "https://x.ai/",
      "https://docs.x.ai/docs/guides?view=desktop#sources",
      "https://example.com/research/findings%20summary.pdf",
      "https://example.com/research/%E2%9C%93?source=grok",
    ]) {
      expect(parseExternalHttpsUrl(url)).toBe(url);
    }
  });

  it("rejects schemes and credential-bearing targets that must never reach the shell", () => {
    for (const url of [
      "http://example.com/",
      "file:///etc/passwd",
      "grok-desktop://open/v1/home",
      "javascript:alert(1)",
      "https://user@example.com/",
      "https://user:secret@example.com/",
      "https://@example.com/",
    ]) {
      expect(() => parseExternalHttpsUrl(url)).toThrow();
    }
  });

  it("rejects every IP-literal spelling, including public and normalized legacy IPv4", () => {
    for (const url of [
      "https://127.0.0.1/",
      "https://10.0.0.1/",
      "https://169.254.169.254/",
      "https://8.8.8.8/",
      "https://[::1]/",
      "https://[2001:4860:4860::8888]/",
      "https://2130706433/",
      "https://0x7f000001/",
    ]) {
      expect(() => parseExternalHttpsUrl(url)).toThrow();
    }
  });

  it("rejects local, private-name, single-label, and internationalized hosts", () => {
    for (const url of [
      "https://localhost/",
      "https://service.localhost/",
      "https://printer.local/",
      "https://service.internal/",
      "https://router.home.arpa/",
      "https://intranet/",
      "https://xn--e1afmkfd.xn--p1ai/",
      "https://éxample.com/",
    ]) {
      expect(() => parseExternalHttpsUrl(url)).toThrow();
    }
  });

  it("rejects noncanonical and ambiguous serializations", () => {
    for (const url of [
      "https://example.com",
      "HTTPS://example.com/",
      "https://EXAMPLE.com/",
      "https://example.com:443/",
      "https://example.com/a/../b",
      "https://example.com/%7euser",
      "https://example.com/%7Euser",
      "https://%65xample.com/",
      "https:\\example.com/",
      " https://example.com/",
      "https://example.com/\n",
      `https://example.com/${"x".repeat(8_192)}`,
    ]) {
      expect(() => parseExternalHttpsUrl(url)).toThrow();
    }
  });

  it("rejects non-string values", () => {
    for (const value of [undefined, null, 1, {}, ["https://example.com/"]]) {
      expect(() => parseExternalHttpsUrl(value)).toThrow("external URL is invalid");
    }
  });
});

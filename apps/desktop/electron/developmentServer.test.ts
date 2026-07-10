import { describe, expect, it } from "vitest";
import { resolveDevelopmentServerUrl } from "./developmentServer.js";

describe("resolveDevelopmentServerUrl", () => {
  it("ignores a development server environment value in packaged builds", () => {
    expect(resolveDevelopmentServerUrl(true, "http://127.0.0.1:5173")).toBeUndefined();
  });

  it("accepts only an exact loopback HTTP origin in development", () => {
    expect(resolveDevelopmentServerUrl(false, "http://127.0.0.1:5173/")).toBe("http://127.0.0.1:5173");
    expect(resolveDevelopmentServerUrl(false, "http://localhost:4173")).toBeUndefined();
    expect(resolveDevelopmentServerUrl(false, "https://127.0.0.1:5173")).toBeUndefined();
    expect(resolveDevelopmentServerUrl(false, "http://example.com:5173")).toBeUndefined();
    expect(resolveDevelopmentServerUrl(false, "http://127.0.0.1:5173/index.html")).toBeUndefined();
    expect(resolveDevelopmentServerUrl(false, "http://127.0.0.1:5173/?token=value")).toBeUndefined();
  });
});

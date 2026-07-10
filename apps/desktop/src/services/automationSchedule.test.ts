import { describe, expect, it } from "vitest";
import { nextAutomationRun, TIME_ZONE_OPTIONS } from "./automationSchedule";
import type { AutomationSchedule } from "./desktopClient";

describe("automation schedules", () => {
  it("uses the runtime's canonical IANA timezone catalog", () => {
    expect(TIME_ZONE_OPTIONS).toContain("UTC");
    expect(TIME_ZONE_OPTIONS).toContain(Intl.DateTimeFormat().resolvedOptions().timeZone);
    expect(new Set(TIME_ZONE_OPTIONS).size).toBe(TIME_ZONE_OPTIONS.length);
  });

  it("calculates the next run in the selected IANA timezone", () => {
    const schedule: AutomationSchedule = { frequency: "weekdays", localTime: "08:30", timeZoneIana: "Europe/Paris" };
    expect(nextAutomationRun(schedule, new Date("2026-07-10T05:00:00Z"))?.toISOString()).toBe("2026-07-10T06:30:00.000Z");
  });

  it("does not silently shift a local time inside a DST gap", () => {
    const schedule: AutomationSchedule = { frequency: "daily", localTime: "02:30", timeZoneIana: "America/New_York" };
    expect(nextAutomationRun(schedule, new Date("2026-03-08T05:00:00Z"))).toBeNull();
  });
});

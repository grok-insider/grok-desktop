import type { AutomationSchedule } from "./desktopClient";

const fallbackTimeZones = [
  "UTC",
  "America/Los_Angeles",
  "America/Denver",
  "America/Chicago",
  "America/New_York",
  "America/Sao_Paulo",
  "Europe/London",
  "Europe/Paris",
  "Europe/Berlin",
  "Europe/Warsaw",
  "Europe/Helsinki",
  "Asia/Dubai",
  "Asia/Kolkata",
  "Asia/Singapore",
  "Asia/Tokyo",
  "Australia/Sydney",
];

type IntlWithSupportedValues = typeof Intl & { supportedValuesOf?: (key: "timeZone") => string[] };

export const TIME_ZONE_OPTIONS = (() => {
  const supportedValuesOf = (Intl as IntlWithSupportedValues).supportedValuesOf;
  const available = supportedValuesOf ? supportedValuesOf.call(Intl, "timeZone") : fallbackTimeZones;
  const canonical = new Set(["UTC", ...available.map(canonicalTimeZone).filter((value): value is string => Boolean(value))]);
  return [...canonical].toSorted((left, right) => left.localeCompare(right));
})();

const weekdayNames = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

export function defaultAutomationSchedule(): AutomationSchedule {
  const detected = canonicalTimeZone(Intl.DateTimeFormat().resolvedOptions().timeZone) ?? "UTC";
  return { frequency: "weekdays", localTime: "08:30", weekday: 1, dayOfMonth: 1, timeZoneIana: TIME_ZONE_OPTIONS.includes(detected) ? detected : "UTC" };
}

export function formatAutomationSchedule(schedule: AutomationSchedule): string {
  const time = formatLocalTime(schedule.localTime);
  const cadence = schedule.frequency === "daily"
    ? "Daily"
    : schedule.frequency === "weekdays"
      ? "Weekdays"
      : schedule.frequency === "weekly"
        ? weekdayNames[schedule.weekday ?? 1]
        : `Day ${schedule.dayOfMonth ?? 1} monthly`;
  return `${cadence} at ${time} · ${schedule.timeZoneIana}`;
}

export function nextAutomationRun(schedule: AutomationSchedule, now = new Date()): Date | null {
  const [hour, minute] = schedule.localTime.split(":").map(Number);
  if (!Number.isInteger(hour) || !Number.isInteger(minute) || hour < 0 || hour > 23 || minute < 0 || minute > 59) return null;
  const localNow = partsInZone(now, schedule.timeZoneIana);
  if (!localNow) return null;

  for (let offset = 0; offset <= 370; offset += 1) {
    const calendarDate = new Date(Date.UTC(localNow.year, localNow.month - 1, localNow.day + offset, 12));
    const weekday = calendarDate.getUTCDay() as AutomationSchedule["weekday"];
    const day = calendarDate.getUTCDate();
    const matches = schedule.frequency === "daily"
      || (schedule.frequency === "weekdays" && weekday !== 0 && weekday !== 6)
      || (schedule.frequency === "weekly" && weekday === (schedule.weekday ?? 1))
      || (schedule.frequency === "monthly" && day === (schedule.dayOfMonth ?? 1));
    if (!matches) continue;
    const candidate = zonedToUtc({
      year: calendarDate.getUTCFullYear(),
      month: calendarDate.getUTCMonth() + 1,
      day,
      hour,
      minute,
    }, schedule.timeZoneIana);
    // A matching local time inside a DST gap is not silently shifted or skipped.
    if (!candidate) return null;
    if (candidate.getTime() > now.getTime() + 30_000) return candidate;
  }
  return null;
}

export function formatNextAutomationRun(schedule: AutomationSchedule, now = new Date()): string {
  const next = nextAutomationRun(schedule, now);
  if (!next) return "Preview unavailable: this local time may fall in a daylight-saving transition.";
  return new Intl.DateTimeFormat(undefined, {
    weekday: "short",
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
    timeZone: schedule.timeZoneIana,
    timeZoneName: "short",
  }).format(next);
}

function canonicalTimeZone(value: string): string | null {
  try {
    return new Intl.DateTimeFormat("en", { timeZone: value }).resolvedOptions().timeZone;
  } catch {
    return null;
  }
}

function formatLocalTime(localTime: string): string {
  const [hour, minute] = localTime.split(":").map(Number);
  const value = new Date(Date.UTC(2020, 0, 1, hour || 0, minute || 0));
  return new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit", timeZone: "UTC" }).format(value);
}

type DateParts = { year: number; month: number; day: number; hour: number; minute: number };

function partsInZone(date: Date, timeZone: string): DateParts | null {
  try {
    const parts = new Intl.DateTimeFormat("en-CA", {
      timeZone,
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
      hourCycle: "h23",
    }).formatToParts(date);
    const get = (type: Intl.DateTimeFormatPartTypes) => Number(parts.find((part) => part.type === type)?.value);
    return { year: get("year"), month: get("month"), day: get("day"), hour: get("hour"), minute: get("minute") };
  } catch {
    return null;
  }
}

function zonedToUtc(target: DateParts, timeZone: string): Date | null {
  let timestamp = Date.UTC(target.year, target.month - 1, target.day, target.hour, target.minute);
  for (let pass = 0; pass < 3; pass += 1) {
    const actual = partsInZone(new Date(timestamp), timeZone);
    if (!actual) return null;
    const targetAsUtc = Date.UTC(target.year, target.month - 1, target.day, target.hour, target.minute);
    const actualAsUtc = Date.UTC(actual.year, actual.month - 1, actual.day, actual.hour, actual.minute);
    timestamp += targetAsUtc - actualAsUtc;
  }
  const resolved = partsInZone(new Date(timestamp), timeZone);
  if (!resolved || Object.keys(target).some((key) => resolved[key as keyof DateParts] !== target[key as keyof DateParts])) return null;
  return new Date(timestamp);
}

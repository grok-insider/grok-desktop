/** Allows only hash routing on the exact renderer document. */
export function isAllowedAppNavigation(target: string, applicationDocument: string): boolean {
  try {
    const candidate = new URL(target);
    const allowed = new URL(applicationDocument);
    return candidate.protocol === allowed.protocol
      && candidate.host === allowed.host
      && candidate.pathname === allowed.pathname
      && candidate.search === allowed.search
      && candidate.username === ""
      && candidate.password === "";
  } catch {
    return false;
  }
}

export function denyRendererPermission(): false {
  return false;
}

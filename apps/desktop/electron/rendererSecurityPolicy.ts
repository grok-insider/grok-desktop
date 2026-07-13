export type ContentSecurityPolicyDelivery = "header" | "meta";

/** Returns the renderer CSP appropriate for its delivery mechanism. */
export function rendererContentSecurityPolicy(
  development: boolean,
  delivery: ContentSecurityPolicyDelivery,
): string {
  const developmentConnect = development ? " ws://127.0.0.1:*" : "";
  const developmentStyle = development ? " 'unsafe-inline'" : "";
  const directives = [
    "default-src 'self'",
    "script-src 'self'",
    `style-src 'self'${developmentStyle}`,
    "style-src-attr 'unsafe-inline'",
    "img-src 'self' data: blob:",
    "font-src 'self'",
    `connect-src 'self'${developmentConnect}`,
    "object-src 'none'",
    "base-uri 'none'",
    "form-action 'none'",
  ];
  if (delivery === "header") directives.push("frame-ancestors 'none'");
  return directives.join("; ");
}

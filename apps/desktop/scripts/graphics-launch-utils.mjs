export const DEVELOPMENT_GRAPHICS_FALLBACK_EXIT_CODE = 78;
export const GRAPHICS_FALLBACK_MARKER = "--grok-graphics-fallback-attempted";
const BACKEND_PREFIX = "--grok-graphics-backend=";

export function softwareFallbackArguments(arguments_) {
  return [
    ...arguments_.filter((argument) =>
      argument !== GRAPHICS_FALLBACK_MARKER && !argument.startsWith(BACKEND_PREFIX)
    ),
    `${BACKEND_PREFIX}software`,
    GRAPHICS_FALLBACK_MARKER,
  ];
}

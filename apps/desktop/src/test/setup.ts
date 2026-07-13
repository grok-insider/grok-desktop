import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

class TestResizeObserver implements ResizeObserver {
  observe(): void {}

  unobserve(): void {}

  disconnect(): void {}
}

if (!globalThis.ResizeObserver) {
  Object.defineProperty(globalThis, "ResizeObserver", {
    configurable: true,
    writable: true,
    value: TestResizeObserver,
  });
}

/*
 * Radix primitives (Select, DropdownMenu, …) require pointer-capture and
 * scroll APIs that jsdom does not implement. The shims below are inert but
 * keep those components mountable and interactable in tests. Electron main
 * tests run in a plain node environment, so every DOM shim is guarded.
 */
const hasDom = typeof MouseEvent !== "undefined" && typeof Element !== "undefined";

if (hasDom && !globalThis.PointerEvent) {
  Object.defineProperty(globalThis, "PointerEvent", {
    configurable: true,
    writable: true,
    value: class PointerEvent extends MouseEvent {
      readonly pointerId: number;

      readonly pointerType: string;

      constructor(type: string, props: PointerEventInit = {}) {
        super(type, props);
        this.pointerId = props.pointerId ?? 0;
        this.pointerType = props.pointerType ?? "mouse";
      }
    },
  });
}

if (hasDom && !Element.prototype.scrollIntoView) {
  Element.prototype.scrollIntoView = () => {};
}
if (hasDom && !Element.prototype.hasPointerCapture) {
  Element.prototype.hasPointerCapture = () => false;
}
if (hasDom && !Element.prototype.setPointerCapture) {
  Element.prototype.setPointerCapture = () => {};
}
if (hasDom && !Element.prototype.releasePointerCapture) {
  Element.prototype.releasePointerCapture = () => {};
}

/* jsdom lacks matchMedia; use-mobile and reduced-motion checks read it. */
if (hasDom && !globalThis.matchMedia) {
  Object.defineProperty(globalThis, "matchMedia", {
    configurable: true,
    writable: true,
    value: (query: string): MediaQueryList => ({
      matches: false,
      media: query,
      onchange: null,
      addListener: () => {},
      removeListener: () => {},
      addEventListener: () => {},
      removeEventListener: () => {},
      dispatchEvent: () => false,
    }),
  });
}

afterEach(cleanup);

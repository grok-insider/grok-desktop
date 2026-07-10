/** True when a normal window close should hide the window instead of ending the process. */
export function shouldHideWindowOnClose(
  keepRunningInNotificationArea: boolean,
  shutdownStarted: boolean,
): boolean {
  return keepRunningInNotificationArea && !shutdownStarted;
}

/** True while Electron must wait for the daemon shutdown handshake before exiting. */
export function shouldDeferAppQuit(
  supervisorAvailable: boolean,
  shutdownCompleted: boolean,
): boolean {
  return supervisorAvailable && !shutdownCompleted;
}

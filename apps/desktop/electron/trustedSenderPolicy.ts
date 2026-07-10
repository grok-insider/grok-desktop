import { isAllowedAppNavigation } from "./navigationPolicy.js";

export interface RendererSenderFacts {
  ownsPrimaryWindow: boolean;
  hasSenderFrame: boolean;
  isTopLevelFrame: boolean;
  frameUrl: string;
}

/** The only renderer identity allowed to request native desktop actions. */
export function isTrustedTopLevelAppSender(
  sender: RendererSenderFacts,
  applicationDocument: string,
): boolean {
  return sender.ownsPrimaryWindow
    && sender.hasSenderFrame
    && sender.isTopLevelFrame
    && isAllowedAppNavigation(sender.frameUrl, applicationDocument);
}

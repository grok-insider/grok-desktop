import { createContext, type ReactNode, useContext, useEffect, useState } from "react";
import type { DesktopClient, DesktopSnapshot } from "./desktopClient";

const DesktopClientContext = createContext<DesktopClient | null>(null);

export function DesktopClientProvider({ client, children }: { client: DesktopClient; children: ReactNode }) {
  return <DesktopClientContext.Provider value={client}>{children}</DesktopClientContext.Provider>;
}

export function useDesktopClient(): DesktopClient {
  const client = useContext(DesktopClientContext);
  if (!client) throw new Error("DesktopClientProvider is missing");
  return client;
}

export function useDesktopSnapshot(): { snapshot: DesktopSnapshot | null; loading: boolean } {
  const client = useDesktopClient();
  const [snapshot, setSnapshot] = useState<DesktopSnapshot | null>(null);

  useEffect(() => {
    let active = true;
    const refresh = () => {
      void client.getSnapshot().then((next) => {
        if (active) setSnapshot(next);
      });
    };
    refresh();
    const unsubscribe = client.subscribe(refresh);
    return () => {
      active = false;
      unsubscribe();
    };
  }, [client]);

  return { snapshot, loading: snapshot === null };
}

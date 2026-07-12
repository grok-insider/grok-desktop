import { useCallback, useEffect, useRef, useState } from "react";
import type { ChatModelCatalog } from "../services/desktopClient";
import { useDesktopClient } from "../services/DesktopClientContext";

const DISCOVERY_ERROR = "The official xAI model catalog is unavailable. Check the account connection and network, then retry.";
const SELECTION_ERROR = "The default model could not be reconciled with live daemon readiness. Retry discovery before changing it again.";

export type ChatModelCatalogStatus = "loading" | "ready" | "error";

export function useChatModelCatalog() {
  const client = useDesktopClient();
  const [catalog, setCatalog] = useState<ChatModelCatalog | null>(null);
  const [status, setStatus] = useState<ChatModelCatalogStatus>("loading");
  const [error, setError] = useState("");
  const [savingModelId, setSavingModelId] = useState<string | null>(null);
  const generation = useRef(0);

  const discover = useCallback(async () => {
    const request = ++generation.current;
    setStatus("loading");
    setError("");
    try {
      const next = await client.getChatModelCatalog();
      if (generation.current !== request) return;
      setCatalog(next);
      setStatus("ready");
    } catch {
      if (generation.current !== request) return;
      setStatus("error");
      setError(DISCOVERY_ERROR);
    }
  }, [client]);

  useEffect(() => {
    setSavingModelId(null);
    void discover();
    return () => {
      generation.current += 1;
    };
  }, [discover]);

  const setDefaultModel = useCallback(async (modelId: string) => {
    if (
      !catalog
      || savingModelId
      || status !== "ready"
      || !catalog.models.some((model) => model.id === modelId && model.textConversationReady)
    ) return false;

    const expectedRevision = catalog.preference.revision;
    const request = generation.current;
    setSavingModelId(modelId);
    setError("");
    try {
      const preference = await client.selectChatModel({ expectedRevision, modelId });
      if (generation.current !== request) return false;
      setCatalog((current) => current && current.preference.revision === expectedRevision
        ? { ...current, preference, selectedModelReady: true }
        : current);
      setStatus("ready");
      return true;
    } catch {
      if (generation.current !== request) return false;
      setStatus("error");
      setError(SELECTION_ERROR);
      return false;
    } finally {
      if (generation.current === request) setSavingModelId(null);
    }
  }, [catalog, client, savingModelId, status]);

  return {
    catalog,
    status,
    error,
    retry: discover,
    setDefaultModel,
    savingModelId,
    selectableModels: catalog?.models.filter((model) => model.textConversationReady) ?? [],
  };
}

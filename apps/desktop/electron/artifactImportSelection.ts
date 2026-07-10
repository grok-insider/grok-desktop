import path from "node:path";

const MAX_SOURCE_PATH_BYTES = 32 * 1024;
const MAX_DISPLAY_NAME_BYTES = 200;

const MEDIA_TYPES: Readonly<Record<string, string>> = {
  ".bmp": "image/bmp",
  ".cjs": "text/javascript",
  ".css": "text/css",
  ".csv": "text/csv",
  ".gif": "image/gif",
  ".htm": "text/html",
  ".html": "text/html",
  ".jpeg": "image/jpeg",
  ".jpg": "image/jpeg",
  ".js": "text/javascript",
  ".json": "application/json",
  ".md": "text/markdown",
  ".mjs": "text/javascript",
  ".mov": "video/quicktime",
  ".mp4": "video/mp4",
  ".pdf": "application/pdf",
  ".png": "image/png",
  ".svg": "image/svg+xml",
  ".ts": "text/plain",
  ".tsv": "text/tab-separated-values",
  ".tsx": "text/plain",
  ".txt": "text/plain",
  ".webm": "video/webm",
  ".webp": "image/webp",
  ".xlsx": "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
  ".xml": "application/xml",
  ".zip": "application/zip",
};

export type ArtifactImportDialogResult = {
  canceled: boolean;
  filePaths: string[];
};

export type ArtifactImportSelection =
  | { kind: "cancelled" }
  | {
      kind: "selected";
      sourcePath: string;
      displayName: string;
      mediaType: string;
    };

/**
 * Converts one native picker result into the bounded, ephemeral daemon input.
 * Errors are deliberately generic so a selected path cannot enter diagnostics.
 */
export function parseArtifactImportDialogResult(
  result: ArtifactImportDialogResult,
  platform: NodeJS.Platform = process.platform,
): ArtifactImportSelection {
  if (result.canceled) {
    if (result.filePaths.length !== 0) throw invalidSelection();
    return { kind: "cancelled" };
  }
  if (result.filePaths.length !== 1) throw invalidSelection();

  const sourcePath = result.filePaths[0];
  const platformPath = platform === "win32" ? path.win32 : path.posix;
  if (
    !sourcePath
    || !platformPath.isAbsolute(sourcePath)
    || Buffer.byteLength(sourcePath, "utf8") > MAX_SOURCE_PATH_BYTES
    || containsControl(sourcePath)
  ) {
    throw invalidSelection();
  }

  const displayName = platformPath.basename(sourcePath);
  if (!isPortableDisplayName(displayName)) throw invalidSelection();
  const extension = platformPath.extname(displayName).toLocaleLowerCase("en-US");
  return {
    kind: "selected",
    sourcePath,
    displayName,
    mediaType: MEDIA_TYPES[extension] ?? "application/octet-stream",
  };
}

function isPortableDisplayName(value: string): boolean {
  if (
    !value
    || value !== value.trim()
    || value.endsWith(".")
    || value.endsWith(" ")
    || value === "."
    || value === ".."
    || Buffer.byteLength(value, "utf8") > MAX_DISPLAY_NAME_BYTES
    || containsControl(value)
    || Array.from(value).some((character) => "<>:\"/\\|?*".includes(character))
  ) {
    return false;
  }
  const deviceStem = value
    .split(".", 1)[0]
    ?.replace(/[ .]+$/u, "")
    .toLocaleUpperCase("en-US");
  return deviceStem !== "CON"
    && deviceStem !== "PRN"
    && deviceStem !== "AUX"
    && deviceStem !== "NUL"
    && deviceStem !== "CLOCK$"
    && !/^COM[1-9]$/u.test(deviceStem ?? "")
    && !/^LPT[1-9]$/u.test(deviceStem ?? "");
}

function containsControl(value: string): boolean {
  return Array.from(value).some((character) => {
    const point = character.codePointAt(0) ?? 0;
    return point <= 0x1f
      || (point >= 0x7f && point <= 0x9f)
      || (point >= 0xd800 && point <= 0xdfff);
  });
}

function invalidSelection(): TypeError {
  return new TypeError("native artifact selection is invalid");
}

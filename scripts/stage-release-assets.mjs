import { constants as fsConstants } from "node:fs";
import { chmod, copyFile, lstat, mkdir, readdir } from "node:fs/promises";
import path from "node:path";
import { pathToFileURL } from "node:url";

const CHANNELS = new Set(["beta", "stable"]);
const MAX_TREE_ENTRIES = 64;

export async function stageReleaseAssets(downloadRoot, outputRoot, channel) {
  if (!CHANNELS.has(channel)) throw new Error("release channel is unsupported");
  const sourceRoot = path.resolve(downloadRoot);
  const destinationRoot = path.resolve(outputRoot);
  if (sourceRoot === destinationRoot || sourceRoot.startsWith(`${destinationRoot}${path.sep}`)
      || destinationRoot.startsWith(`${sourceRoot}${path.sep}`)) {
    throw new Error("release staging roots must be separate");
  }
  const expected = [
    record("apps/desktop/release/components/grok-build/linux-x64.json", "grok-build-linux-x64.json"),
    record("apps/desktop/release/components/grok-build/windows-x64.json", "grok-build-windows-x64.json"),
    record(`out/release/linux/x64/GrokDesktop-${channel}-x64.AppImage`, `GrokDesktop-${channel}-x64.AppImage`, 0o700),
    record(`out/release/linux/x64/GrokDesktop-${channel}-x64.AppImage.zsync`, `GrokDesktop-${channel}-x64.AppImage.zsync`),
    record("out/release/linux/x64/linux-package.json", "linux-package.json"),
    record(`out/release/windows/${channel}/x64/GrokDesktop-${channel}-x64.exe`, `GrokDesktop-${channel}-x64.exe`, 0o700),
    record(`out/release/windows/${channel}/x64/windows-package.json`, "windows-package.json"),
  ];
  const actual = await collectRegularFiles(sourceRoot);
  const approved = expected.map(({ source }) => source).toSorted();
  if (actual.length !== approved.length
      || actual.some((candidate, index) => candidate !== approved[index])) {
    throw new Error("downloaded release artifacts do not match the exact allowlist");
  }
  await mkdir(destinationRoot, { mode: 0o700 });
  for (const item of expected) {
    const source = path.join(sourceRoot, ...item.source.split("/"));
    const destination = path.join(destinationRoot, item.destination);
    await copyFile(source, destination, fsConstants.COPYFILE_EXCL);
    await chmod(destination, item.mode);
  }
}

function record(source, destination, mode = 0o600) {
  return { source, destination, mode };
}

async function collectRegularFiles(root) {
  const rootMetadata = await lstat(root);
  if (!rootMetadata.isDirectory() || rootMetadata.isSymbolicLink()) {
    throw new Error("release artifact download root is invalid");
  }
  const files = [];
  let entriesSeen = 0;
  async function visit(directory, relativeDirectory) {
    const entries = await readdir(directory, { withFileTypes: true });
    for (const entry of entries.toSorted((left, right) => left.name.localeCompare(right.name, "en"))) {
      entriesSeen += 1;
      if (entriesSeen > MAX_TREE_ENTRIES) throw new Error("release artifact tree is too large");
      const relative = relativeDirectory ? `${relativeDirectory}/${entry.name}` : entry.name;
      const absolute = path.join(directory, entry.name);
      const metadata = await lstat(absolute);
      if (metadata.isSymbolicLink()) throw new Error("release artifact tree contains a link");
      if (metadata.isDirectory()) {
        await visit(absolute, relative);
      } else if (metadata.isFile() && metadata.size > 0) {
        files.push(relative);
      } else {
        throw new Error("release artifact tree contains an invalid entry");
      }
    }
  }
  await visit(root, "");
  return files.toSorted();
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  if (process.argv.length !== 5) {
    throw new Error("usage: node scripts/stage-release-assets.mjs <download-root> <output-root> <channel>");
  }
  await stageReleaseAssets(process.argv[2], process.argv[3], process.argv[4]);
}

import { cp, mkdtemp, mkdir, readFile, rm, stat, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { packager } from "@electron/packager";
import { packageMSIX } from "electron-windows-msix";

import {
  createSigningToolEnvironment,
  normalizeMsixVersion,
  parseReleaseArguments,
  readCoreWindowsReleaseEnvironment,
  renderManifest,
  renderStableAppInstaller,
  sha256File,
  validateCoreWindowsInputs,
  verifyPackagedCoreWindowsLayout,
} from "./release-utils.mjs";
import {
  assertReleaseBuildExists,
  hardenElectronExecutable,
  preparePackagingSource,
  readableFuseState,
  signAndVerifyDirectory,
  signArtifact,
  verifySignature,
} from "./package-windows.mjs";

const desktopRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const repositoryRoot = path.resolve(desktopRoot, "../..");
const productName = "Grok Desktop";

async function main() {
  if (process.platform !== "win32") {
    throw new Error("Windows packages must be assembled and signed on a Windows release worker");
  }
  const releaseArguments = parseReleaseArguments(process.argv.slice(2));
  if (releaseArguments.architecture !== "x64") {
    throw new Error("the public core package currently supports Windows x64 only");
  }
  const environment = readCoreWindowsReleaseEnvironment(process.env);
  const packageMetadata = JSON.parse(await readFile(path.join(desktopRoot, "package.json"), "utf8"));
  const msixVersion = normalizeMsixVersion(packageMetadata.version, releaseArguments.channel);
  const stageRoot = path.resolve(releaseArguments.stage ??
    path.join(repositoryRoot, "out", "release-inputs", "windows-core", "x64"));
  const outputRoot = path.resolve(releaseArguments.out ??
    path.join(repositoryRoot, "out", "release", "windows", releaseArguments.channel, "x64"));
  const inputs = await validateCoreWindowsInputs(stageRoot, { architecture: "x64" });
  await assertReleaseBuildExists();
  const signingEnvironment = createSigningToolEnvironment(process.env);

  const temporaryRoot = await mkdtemp(path.join(os.tmpdir(), "grok-desktop-core-release-"));
  try {
    const sourceRoot = path.join(temporaryRoot, "source");
    await preparePackagingSource(sourceRoot, packageMetadata, environment.updateTrustedKeysJSON);
    await rm(outputRoot, { recursive: true, force: true });
    await mkdir(outputRoot, { recursive: true });
    const packagedDirectories = await packager({
      dir: sourceRoot,
      out: path.join(outputRoot, "unpacked"),
      overwrite: true,
      platform: "win32",
      arch: "x64",
      name: productName,
      executableName: productName,
      appVersion: packageMetadata.version,
      buildVersion: msixVersion,
      appCopyright: "Copyright (c) 2026 Grok Insider",
      asar: true,
      prune: true,
      derefSymlinks: false,
      icon: path.join(desktopRoot, "release", "windows", "assets", "icon.ico"),
      extraResource: [
        path.join(sourceRoot, "update-trusted-keys.json"),
        path.join(desktopRoot, "assets", "tray"),
        path.join(inputs.canonicalRoot, "bin"),
      ],
    });
    if (packagedDirectories.length !== 1) {
      throw new Error("Electron Packager returned an unexpected target set");
    }
    const appDirectory = packagedDirectories[0];
    const executable = path.join(appDirectory, `${productName}.exe`);
    await verifyPackagedCoreWindowsLayout(appDirectory, inputs, "x64");
    await hardenElectronExecutable(executable);
    await signAndVerifyDirectory(appDirectory, environment, signingEnvironment);
    await verifyPackagedCoreWindowsLayout(
      appDirectory, inputs, "x64", { firstPartyBinariesSigned: true },
    );

    const manifestTemplate = await readFile(
      path.join(desktopRoot, "release", "windows", "AppxManifest.xml.template"), "utf8",
    );
    const appxManifest = renderManifest(manifestTemplate, {
      PACKAGE_IDENTITY: environment.packageIdentity,
      PUBLISHER: environment.publisher,
      PACKAGE_VERSION: msixVersion,
      ARCHITECTURE: "x64",
      PUBLISHER_DISPLAY_NAME: environment.publisherDisplayName,
      MAX_TESTED_VERSION: environment.maxTestedVersion,
    });
    const appxManifestPath = path.join(temporaryRoot, "AppxManifest.xml");
    await writeFile(appxManifestPath, appxManifest, { encoding: "utf8", mode: 0o600 });
    const packageName =
      `GrokDesktop-${packageMetadata.version}-${releaseArguments.channel}-x64.msix`;
    const { msixPackage } = await packageMSIX({
      appDir: appDirectory,
      appManifest: appxManifestPath,
      packageAssets: path.join(desktopRoot, "release", "windows", "assets"),
      outputDir: outputRoot,
      packageName,
      createPri: true,
      sign: false,
      logLevel: "warn",
    });
    await signArtifact(msixPackage, environment, signingEnvironment);
    const signer = await verifySignature(msixPackage, environment, signingEnvironment);
    const metadata = await stat(msixPackage);
    const releaseRecord = {
      schemaVersion: 4,
      product: "grok-desktop",
      capabilityProfile: "core-host-tools-beta",
      deferredCapabilities: ["isolated-work", "media", "browser-automation", "scheduled-work"],
      version: packageMetadata.version,
      msixVersion,
      channel: releaseArguments.channel,
      architecture: "x64",
      packageIdentity: environment.packageIdentity,
      officialGrokComponent: {
        version: inputs.manifest.version,
        sourceUrl: inputs.manifest.sourceUrl,
        sha256: inputs.manifest.sha256,
        size: inputs.manifest.size,
        trustBinding: inputs.manifest.binding,
        authenticodePolicy: "preserve-vendor-signature-do-not-resign",
      },
      signer,
      artifact: {
        file: path.basename(msixPackage), size: metadata.size, sha256: await sha256File(msixPackage),
      },
      fuses: await readableFuseState(executable),
    };
    await writeFile(
      path.join(outputRoot, `${packageName}.json`), `${JSON.stringify(releaseRecord, null, 2)}\n`,
      { encoding: "utf8", mode: 0o600 },
    );
    await cp(msixPackage, path.join(outputRoot, `GrokDesktop-${releaseArguments.channel}-x64.msix`), {
      errorOnExist: true,
    });
    if (releaseArguments.channel === "stable") {
      await writeFile(
        path.join(outputRoot, "GrokDesktop-stable-x64.appinstaller"),
        renderStableAppInstaller({
          architecture: "x64",
          packageIdentity: environment.packageIdentity,
          publisher: environment.publisher,
          version: msixVersion,
        }),
        { encoding: "utf8", mode: 0o600, flag: "wx" },
      );
    }
  } finally {
    await rm(temporaryRoot, { recursive: true, force: true });
  }
}

await main();

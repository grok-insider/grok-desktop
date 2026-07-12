import { spawn } from "node:child_process";
import { cp, mkdtemp, mkdir, readFile, readdir, rm, stat, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { flipFuses, FuseState, FuseV1Options, FuseVersion, getCurrentFuseWire } from "@electron/fuses";
import { packager } from "@electron/packager";
import { packageMSIX } from "electron-windows-msix";
import {
  createSigningToolEnvironment,
  normalizeMsixVersion,
  parseReleaseArguments,
  readReleaseEnvironment,
  renderManifest,
  renderStableAppInstaller,
  sha256File,
  shouldAuthenticodeSignPackagedFile,
  validateSignerIdentity,
  validateReleaseInputs,
  verifyPackagedNativeLayout,
} from "./release-utils.mjs";

const desktopRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const repositoryRoot = path.resolve(desktopRoot, "../..");
const productName = "Grok Desktop";

async function main() {
  if (process.platform !== "win32") throw new Error("Windows packages must be assembled and signed on a Windows release worker");
  const releaseArguments = parseReleaseArguments(process.argv.slice(2));
  const environment = readReleaseEnvironment(process.env);
  const packageMetadata = JSON.parse(await readFile(path.join(desktopRoot, "package.json"), "utf8"));
  const msixVersion = normalizeMsixVersion(packageMetadata.version);
  const stageRoot = path.resolve(releaseArguments.stage ?? path.join(repositoryRoot, "out", "release-inputs", "windows", releaseArguments.architecture));
  const outputRoot = path.resolve(releaseArguments.out ?? path.join(repositoryRoot, "out", "release", "windows", releaseArguments.channel, releaseArguments.architecture));
  const inputs = await validateReleaseInputs(stageRoot, {
    architecture: releaseArguments.architecture,
    channel: releaseArguments.channel,
    desktopVersion: packageMetadata.version,
    releaseMetadataKeys: environment.releaseMetadataKeys,
    acpCatalogTrust: environment.acpCatalogTrust,
  });
  await assertReleaseBuildExists();
  const signingToolEnvironment = createSigningToolEnvironment(process.env);

  const temporaryRoot = await mkdtemp(path.join(os.tmpdir(), "grok-desktop-release-"));
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
      arch: releaseArguments.architecture,
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
        path.dirname(inputs.files.get("bin/grok-daemon.exe")),
        path.dirname(inputs.files.get("service/grok-vm-service.exe")),
        path.dirname(inputs.files.get("guest/grok-guest.vhdx")),
        path.dirname(inputs.files.get("catalog/components.json")),
      ],
    });
    if (packagedDirectories.length !== 1) throw new Error("Electron Packager returned an unexpected target set");
    const appDirectory = packagedDirectories[0];
    const executable = path.join(appDirectory, `${productName}.exe`);
    await verifyPackagedNativeLayout(appDirectory, inputs, releaseArguments.architecture);
    await hardenElectronExecutable(executable);

    await signAndVerifyDirectory(appDirectory, environment, signingToolEnvironment);
    await verifyPackagedNativeLayout(
      appDirectory, inputs, releaseArguments.architecture, { firstPartyBinariesSigned: true },
    );

    const manifestTemplate = await readFile(path.join(desktopRoot, "release", "windows", "AppxManifest.xml.template"), "utf8");
    const manifest = renderManifest(manifestTemplate, {
      PACKAGE_IDENTITY: environment.packageIdentity,
      PUBLISHER: environment.publisher,
      PACKAGE_VERSION: msixVersion,
      ARCHITECTURE: releaseArguments.architecture,
      PUBLISHER_DISPLAY_NAME: environment.publisherDisplayName,
      MAX_TESTED_VERSION: environment.maxTestedVersion,
    });
    const manifestPath = path.join(temporaryRoot, "AppxManifest.xml");
    await writeFile(manifestPath, manifest, { encoding: "utf8", mode: 0o600 });

    const packageName = `GrokDesktop-${packageMetadata.version}-${releaseArguments.channel}-${releaseArguments.architecture}.msix`;
    const { msixPackage } = await packageMSIX({
      appDir: appDirectory,
      appManifest: manifestPath,
      packageAssets: path.join(desktopRoot, "release", "windows", "assets"),
      outputDir: outputRoot,
      packageName,
      createPri: true,
      sign: false,
      logLevel: "warn",
    });
    await signArtifact(msixPackage, environment, signingToolEnvironment);
    const signer = await verifySignature(msixPackage, environment, signingToolEnvironment);

    const msixMetadata = await stat(msixPackage);
    const releaseRecord = {
      schemaVersion: 3,
      product: "grok-desktop",
      version: packageMetadata.version,
      msixVersion,
      channel: releaseArguments.channel,
      architecture: releaseArguments.architecture,
      packageIdentity: environment.packageIdentity,
      sourceInputManifestSha256: await sha256File(path.join(inputs.canonicalRoot, "release-inputs.json")),
      sourceInput: {
        sequence: inputs.manifest.sequence,
        signatureKeyId: inputs.manifest.signature.keyId,
        guestImageId: inputs.manifest.guest.imageId,
        guestImageVersion: inputs.manifest.guest.imageVersion,
        guestStagingName: inputs.manifest.guest.stagingName,
        guestCatalogSequence: inputs.guestCatalog.sequence,
        guestCatalogSignatureKeyId: inputs.guestCatalog.signature.keyId,
        acpCatalogSequence: inputs.acpCatalog.sequence,
        acpCatalogExpiresAtUnixSeconds: inputs.acpCatalog.expiresAtUnixSeconds,
        acpCatalogSignatureKeyId: inputs.acpCatalog.signatureKeyId,
        acpComponentVersion: inputs.acpComponent.version,
        acpComponentPath: inputs.acpComponent.stagePath,
        acpComponentSha256: inputs.acpComponent.sha256,
        acpComponentAuthenticodePolicy: "preserve-vendor-signature-do-not-resign",
        acpComponentProvenanceEvidenceId: environment.acpProvenanceEvidenceID,
        acpComponentRedistributionEvidenceId: environment.acpRedistributionEvidenceID,
      },
      signer,
      artifact: { file: path.basename(msixPackage), size: msixMetadata.size, sha256: await sha256File(msixPackage) },
      fuses: await readableFuseState(executable),
    };
    await writeFile(path.join(outputRoot, `${packageName}.json`), `${JSON.stringify(releaseRecord, null, 2)}\n`, { encoding: "utf8", mode: 0o600 });
    if (releaseArguments.channel === "stable") {
      const stablePackageName = `GrokDesktop-stable-${releaseArguments.architecture}.msix`;
      await cp(msixPackage, path.join(outputRoot, stablePackageName), { errorOnExist: true });
      await writeFile(
        path.join(outputRoot, `GrokDesktop-stable-${releaseArguments.architecture}.appinstaller`),
        renderStableAppInstaller({
          architecture: releaseArguments.architecture,
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

async function preparePackagingSource(sourceRoot, packageMetadata, updateTrustedKeysJSON) {
  await mkdir(path.join(sourceRoot, "node_modules", "@bufbuild"), { recursive: true });
  await cp(path.join(desktopRoot, "dist"), path.join(sourceRoot, "dist"), { recursive: true, dereference: false, errorOnExist: true });
  await cp(path.join(desktopRoot, "dist-electron"), path.join(sourceRoot, "dist-electron"), { recursive: true, dereference: false, errorOnExist: true });
  await writeFile(path.join(sourceRoot, "update-trusted-keys.json"), `${updateTrustedKeysJSON}\n`, {
    encoding: "utf8", mode: 0o600, flag: "wx",
  });
  await cp(
    path.join(desktopRoot, "node_modules", "@bufbuild", "protobuf"),
    path.join(sourceRoot, "node_modules", "@bufbuild", "protobuf"),
    { recursive: true, dereference: true, errorOnExist: true },
  );
  const packagedMetadata = {
    name: "grok-desktop",
    productName,
    version: packageMetadata.version,
    description: packageMetadata.description,
    private: true,
    type: "module",
    main: "dist-electron/electron/main.js",
    license: "AGPL-3.0-or-later",
    dependencies: { "@bufbuild/protobuf": packageMetadata.dependencies["@bufbuild/protobuf"] },
  };
  await writeFile(path.join(sourceRoot, "package.json"), `${JSON.stringify(packagedMetadata, null, 2)}\n`, { encoding: "utf8", mode: 0o600 });
}

async function assertReleaseBuildExists() {
  for (const relativePath of ["dist/index.html", "dist-electron/electron/main.js", "dist-electron/electron/preload.cjs"]) {
    const metadata = await stat(path.join(desktopRoot, relativePath)).catch(() => undefined);
    if (!metadata?.isFile()) throw new Error("run the production desktop build before packaging");
  }
  for (const root of [path.join(desktopRoot, "dist"), path.join(desktopRoot, "dist-electron")]) {
    for (const file of await walkFiles(root)) if (file.endsWith(".map")) throw new Error("release output must not contain source maps");
  }
}

async function hardenElectronExecutable(executable) {
  await flipFuses(executable, {
    version: FuseVersion.V1,
    strictlyRequireAllFuses: true,
    [FuseV1Options.RunAsNode]: false,
    [FuseV1Options.EnableCookieEncryption]: true,
    [FuseV1Options.EnableNodeOptionsEnvironmentVariable]: false,
    [FuseV1Options.EnableNodeCliInspectArguments]: false,
    [FuseV1Options.EnableEmbeddedAsarIntegrityValidation]: true,
    [FuseV1Options.OnlyLoadAppFromAsar]: true,
    [FuseV1Options.LoadBrowserProcessSpecificV8Snapshot]: false,
    [FuseV1Options.GrantFileProtocolExtraPrivileges]: false,
    [FuseV1Options.WasmTrapHandlers]: true,
  });
  const state = await getCurrentFuseWire(executable);
  const expected = new Map([
    [FuseV1Options.RunAsNode, FuseState.DISABLE],
    [FuseV1Options.EnableCookieEncryption, FuseState.ENABLE],
    [FuseV1Options.EnableNodeOptionsEnvironmentVariable, FuseState.DISABLE],
    [FuseV1Options.EnableNodeCliInspectArguments, FuseState.DISABLE],
    [FuseV1Options.EnableEmbeddedAsarIntegrityValidation, FuseState.ENABLE],
    [FuseV1Options.OnlyLoadAppFromAsar, FuseState.ENABLE],
    [FuseV1Options.LoadBrowserProcessSpecificV8Snapshot, FuseState.DISABLE],
    [FuseV1Options.GrantFileProtocolExtraPrivileges, FuseState.DISABLE],
    [FuseV1Options.WasmTrapHandlers, FuseState.ENABLE],
  ]);
  for (const [fuse, value] of expected) if (state[fuse] !== value) throw new Error("packaged Electron fuse verification failed");
}

async function readableFuseState(executable) {
  const state = await getCurrentFuseWire(executable);
  return Object.fromEntries(Object.entries(FuseV1Options)
    .filter(([name]) => Number.isNaN(Number(name)))
    .map(([name, index]) => [name, state[index] === FuseState.ENABLE]));
}

async function signAndVerifyDirectory(root, environment, toolEnvironment) {
  const signable = (await walkFiles(root)).filter((file) =>
    shouldAuthenticodeSignPackagedFile(root, file));
  if (signable.length === 0) throw new Error("packaged application contains no signable binaries");
  for (const file of signable) {
    await signArtifact(file, environment, toolEnvironment);
    await verifySignature(file, environment, toolEnvironment);
  }
}

async function signArtifact(file, environment, toolEnvironment) {
  await spawnChecked(environment.signToolPath, [
    "sign",
    "/fd", "sha256",
    "/tr", environment.timestampServer,
    "/td", "sha256",
    "/d", productName,
    "/du", "https://github.com/grok-insider/grok-desktop",
    ...environment.signingArguments,
    file,
  ], toolEnvironment);
}

async function verifySignature(file, environment, toolEnvironment) {
  await spawnChecked(environment.signToolPath, ["verify", "/pa", "/all", file], toolEnvironment);
  const signerJSON = await spawnChecked(environment.powershellPath, [
    "-NoLogo",
    "-NoProfile",
    "-NonInteractive",
    "-Command",
    "$ErrorActionPreference='Stop';$signature=Get-AuthenticodeSignature -LiteralPath $env:GROK_SIGNATURE_FILE;if($signature.Status.ToString() -ne 'Valid' -or $null -eq $signature.SignerCertificate -or $null -eq $signature.TimeStamperCertificate){throw 'Authenticode signer or timestamp is unavailable'};$record=[ordered]@{subject=$signature.SignerCertificate.Subject;thumbprint=$signature.SignerCertificate.Thumbprint};[Console]::Out.Write(($record|ConvertTo-Json -Compress))",
  ], createSigningToolEnvironment(toolEnvironment, { GROK_SIGNATURE_FILE: file }));
  let signer;
  try { signer = JSON.parse(signerJSON); } catch { throw new Error("PowerShell returned invalid signer identity JSON"); }
  return validateSignerIdentity(signer, environment.publisher, environment.signerThumbprint);
}

async function walkFiles(root) {
  const output = [];
  for (const entry of await readdir(root, { withFileTypes: true })) {
    const candidate = path.join(root, entry.name);
    if (entry.isSymbolicLink()) throw new Error("release output contains a symbolic link");
    if (entry.isDirectory()) output.push(...await walkFiles(candidate));
    else if (entry.isFile()) output.push(candidate);
    else throw new Error("release output contains an unsupported file type");
  }
  return output;
}

function spawnChecked(command, arguments_, environment) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, arguments_, {
      env: environment,
      shell: false,
      windowsHide: true,
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk) => { if (stdout.length < 16_384) stdout += chunk; });
    child.stderr.on("data", (chunk) => { if (stderr.length < 16_384) stderr += chunk; });
    child.once("error", () => reject(new Error("release verification tool could not be started")));
    child.once("exit", (code) => code === 0 ? resolve(stdout) : reject(new Error(`release verification tool failed with code ${code}: ${stderr.slice(0, 512)}`)));
  });
}

await main();

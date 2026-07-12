import assert from "node:assert/strict";
import { createHash, generateKeyPairSync, sign as signData } from "node:crypto";
import { cp, mkdtemp, mkdir, readFile, rm, symlink, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import {
  createSigningToolEnvironment,
  guestImageCatalogSigningBytes,
  inspectDaemonAcpCatalogTrust,
  inspectPortableExecutable,
  inspectServiceGuestCatalogTrust,
  normalizeMsixVersion,
  officialGrokCatalogSignatureBytes,
  parseAcpCatalogTrustedKeys,
  parseIntegrationCatalog,
  parseReleaseArguments,
  parseReleaseMetadataKeys,
  readReleaseEnvironment,
  releaseInputSigningBytes,
  renderStableAppInstaller,
  renderManifest,
  serviceGuestCatalogTrust,
  shouldAuthenticodeSignPackagedFile,
  validateReleaseInputs,
  validateSignerIdentity,
  verifyOfficialGrokCatalog,
  verifyPackagedNativeLayout,
  windowsServiceBuildMetadata,
} from "./release-utils.mjs";
import {
  assertNoCargoConfigurationInAncestors,
  createWindowsDaemonBuildEnvironment,
  parseDaemonBuildArguments,
  parseWindowsToolchainEnvironment,
  prepareIsolatedCargoHome,
} from "./build-windows-daemon.mjs";
import {
  createWindowsServiceBuildEnvironment,
  parseServiceBuildArguments,
} from "./build-windows-service.mjs";

const releaseKeyID = "release-test-2026";
const releaseKeys = generateKeyPairSync("ed25519");
const releasePublicKey = releaseKeys.publicKey.export({ format: "der", type: "spki" }).toString("base64");
const trustedReleaseKeys = parseReleaseMetadataKeys(JSON.stringify({ [releaseKeyID]: releasePublicKey }));
const acpKeyID = "xai-acp-test-2026";
const acpKeys = generateKeyPairSync("ed25519");
const acpPublicRaw = Buffer.from(acpKeys.publicKey.export({ format: "jwk" }).x, "base64url").toString("hex");
const acpTrustRaw = `${acpKeyID}=${acpPublicRaw}`;
const trustedAcpKeys = parseAcpCatalogTrustedKeys(acpTrustRaw);
const signerThumbprint = "0123456789ABCDEF0123456789ABCDEF01234567";
const acpNow = 1_800_000_000;

test("normalizes versions and parses explicit release targets", () => {
  assert.equal(normalizeMsixVersion("12.34.56"), "12.34.56.0");
  assert.throws(() => normalizeMsixVersion("1.2.3-beta.1"), /prerelease/);
  assert.throws(() => normalizeMsixVersion("1.70000.0"), /component limit/);
  assert.deepEqual(parseReleaseArguments(["--arch", "arm64", "--channel", "stable"]), {
    architecture: "arm64", channel: "stable", stage: undefined, out: undefined,
  });
  assert.throws(() => parseReleaseArguments(["--arch", "ia32", "--channel", "stable"]), /x64 or arm64/);
});

test("renders stable App Installer metadata with fixed identity and update origin", () => {
  const appInstaller = renderStableAppInstaller({
    architecture: "x64",
    packageIdentity: "GrokDesktop.Test",
    publisher: "CN=Grok Desktop Test",
    version: "1.2.3.0",
  });
  assert.match(appInstaller, /appinstaller\/2021/);
  assert.match(appInstaller, /releases\/latest\/download\/GrokDesktop-stable-x64\.msix/);
  assert.match(appInstaller, /AutomaticBackgroundTask/);
  assert.match(appInstaller, /UpdateBlocksActivation="false"/);
  assert.throws(() => renderStableAppInstaller({
    architecture: "ia32", packageIdentity: "GrokDesktop.Test", publisher: "CN=Test", version: "1.2.3.0",
  }), /architecture/);
});

test("creates isolated native build environments and deterministic public trust bindings", async () => {
  const serviceTrust = serviceGuestCatalogTrust(trustedReleaseKeys);
  assert.match(serviceTrust.binding, /^grok-guest-catalog-trust-v1:[a-f0-9]{64}$/);
  const serviceBuild = windowsServiceBuildMetadata("1.2.3", trustedReleaseKeys);
  assert.match(serviceBuild.linkerFlags, /main\.guestCatalogTrust=/);
  assert.deepEqual(parseServiceBuildArguments([
    "--arch", "arm64", "--out", "stage/grok-vm-service.exe",
  ]), { architecture: "arm64", output: path.resolve("stage/grok-vm-service.exe") });
  const serviceEnvironment = createWindowsServiceBuildEnvironment({
    GOFLAGS: "-toolexec=untrusted.exe", GOENV: "C:\\untrusted\\go.env",
    Path: "C:\\untrusted-bin", SystemRoot: "C:\\Windows",
  }, "x64");
  assert.equal(serviceEnvironment.GOFLAGS, "");
  assert.equal(serviceEnvironment.GOENV, "off");
  assert.equal(serviceEnvironment.Path, undefined);

  assert.match(trustedAcpKeys.binding, /^grok-acp-catalog-trust-v1:[a-f0-9]{64}$/);
  assert.equal(trustedAcpKeys.keys.get(acpKeyID).asymmetricKeyType, "ed25519");
  assert.throws(() => parseAcpCatalogTrustedKeys(`z=${acpPublicRaw};a=${acpPublicRaw}`), /ordered/);
  assert.throws(() => parseAcpCatalogTrustedKeys(`${acpKeyID}=${acpPublicRaw.toUpperCase()}`), /ordered/);
  assert.deepEqual(parseDaemonBuildArguments([
    "--arch", "x64", "--out", "stage/grok-daemon.exe",
  ]), { architecture: "x64", output: path.resolve("stage/grok-daemon.exe") });
  const daemonBuildRoot = path.resolve(os.tmpdir(), "grok-daemon-build-test");
  const layout = {
    cargoHome: path.join(daemonBuildRoot, "cargo-home"),
    homeDirectory: path.join(daemonBuildRoot, "home"),
    targetDirectory: path.join(daemonBuildRoot, "target"),
    temporaryDirectory: path.join(daemonBuildRoot, "tmp"),
    workingDirectory: path.join(daemonBuildRoot, "work"),
  };
  const toolchainEnvironment = parseWindowsToolchainEnvironment(JSON.stringify({
    systemRoot: "C:\\Windows",
    executablePaths: ["C:\\Rust\\bin", "C:\\BuildTools\\bin"],
    includePaths: ["C:\\BuildTools\\include"],
    libraryPaths: ["C:\\BuildTools\\lib"],
    librarySearchPaths: ["C:\\BuildTools\\libpath"],
  }));
  const daemonEnvironment = createWindowsDaemonBuildEnvironment({
    Path: "C:\\trusted-toolchain",
    RUSTFLAGS: "--cfg untrusted",
    CARGO_ENCODED_RUSTFLAGS: "untrusted",
    CARGO_HOME: "C:\\untrusted-cargo-home",
    HOME: "C:\\Users\\untrusted",
    SystemRoot: "C:\\Windows",
  }, "x64", layout, trustedAcpKeys, {
    rustcPath: "C:\\Rust\\bin\\rustc.exe",
    linkerPath: "C:\\BuildTools\\bin\\link.exe",
    toolchainEnvironment,
  });
  assert.equal(daemonEnvironment.GROK_ACP_CATALOG_TRUSTED_KEYS, acpTrustRaw);
  assert.equal(daemonEnvironment.GROK_ACP_CATALOG_TRUST_BINDING, trustedAcpKeys.binding);
  assert.equal(daemonEnvironment.CARGO_HOME, layout.cargoHome);
  assert.equal(daemonEnvironment.CARGO_NET_OFFLINE, "true");
  assert.equal(daemonEnvironment.HOME, layout.homeDirectory);
  assert.equal(daemonEnvironment.PATH, "C:\\Rust\\bin;C:\\BuildTools\\bin");
  assert.equal(daemonEnvironment.RUSTC, "C:\\Rust\\bin\\rustc.exe");
  assert.equal(
    daemonEnvironment.CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER,
    "C:\\BuildTools\\bin\\link.exe",
  );
  assert.equal(daemonEnvironment.RUSTFLAGS, undefined);
  assert.equal(daemonEnvironment.CARGO_ENCODED_RUSTFLAGS, undefined);
  assert.equal(daemonEnvironment.Path, undefined);
  const daemonBuildSource = await readFile(new URL("./build-windows-daemon.mjs", import.meta.url), "utf8");
  assert.match(daemonBuildSource, /"--locked"[\s\S]*?"--offline"[\s\S]*?"--no-default-features"/);
  assert.match(daemonBuildSource, /"--manifest-path", path\.join\(repositoryRoot, "Cargo\.toml"\)/);
  assert.doesNotMatch(daemonBuildSource, /--features[\s\S]*debug-acp-descriptor/);
});

test("rejects ambient Cargo configuration and malformed Windows toolchain contracts", async (t) => {
  assert.throws(() => parseWindowsToolchainEnvironment(
    '{"systemRoot":"C:\\\\Windows","systemRoot":"C:\\\\Other"}',
  ), /strict JSON/);
  assert.throws(() => parseWindowsToolchainEnvironment(JSON.stringify({
    systemRoot: "C:\\Windows",
    executablePaths: ["C:\\Rust\\bin", "c:\\rust\\bin"],
    includePaths: ["C:\\BuildTools\\include"],
    libraryPaths: ["C:\\BuildTools\\lib"],
    librarySearchPaths: ["C:\\BuildTools\\libpath"],
  })), /unique/);
  assert.throws(() => parseWindowsToolchainEnvironment(JSON.stringify({
    systemRoot: "C:\\Windows",
    executablePaths: ["\\\\worker\\share\\bin"],
    includePaths: ["C:\\BuildTools\\include"],
    libraryPaths: ["C:\\BuildTools\\lib"],
    librarySearchPaths: ["C:\\BuildTools\\libpath"],
  })), /absolute/);

  const root = await mkdtemp(path.join(os.tmpdir(), "grok-cargo-isolation-test-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const cache = path.join(root, "trusted-cache");
  const registryCache = path.join(cache, "registry", "cache");
  const registryIndex = path.join(cache, "registry", "index");
  await mkdir(registryCache, { recursive: true });
  await mkdir(registryIndex, { recursive: true });
  await writeFile(path.join(registryCache, "fixture.crate"), "fixture");
  await writeFile(path.join(registryIndex, "config.json"), "{}");
  const isolated = path.join(root, "isolated-cargo-home");
  await prepareIsolatedCargoHome(cache, isolated);
  assert.equal(await readFile(path.join(isolated, "registry", "cache", "fixture.crate"), "utf8"), "fixture");
  assert.equal(await readFile(path.join(isolated, "registry", "index", "config.json"), "utf8"), "{}");

  await writeFile(path.join(cache, "config.toml"), "[build]\nrustc-wrapper='untrusted'\n");
  await assert.rejects(
    prepareIsolatedCargoHome(cache, path.join(root, "configured-cargo-home")),
    /must not contain Cargo configuration/,
  );
  await rm(path.join(cache, "config.toml"));
  try {
    await symlink(path.join(registryCache, "fixture.crate"), path.join(registryCache, "linked.crate"));
    await assert.rejects(
      prepareIsolatedCargoHome(cache, path.join(root, "linked-cargo-home")),
      /symbolic link/,
    );
  } catch (error) {
    if (process.platform !== "win32" || error?.code !== "EPERM") throw error;
  }

  const workingDirectory = path.join(root, "parent", "work");
  await mkdir(workingDirectory, { recursive: true });
  await assertNoCargoConfigurationInAncestors(workingDirectory);
  await mkdir(path.join(root, "parent", ".cargo"));
  await writeFile(path.join(root, "parent", ".cargo", "config.toml"), "[env]\nINJECTED='1'\n");
  await assert.rejects(
    assertNoCargoConfigurationInAncestors(workingDirectory),
    /Cargo configuration is forbidden/,
  );
});

test("keeps guest catalog canonical signing bytes compatible with the service", () => {
  const catalog = {
    schemaVersion: 1,
    product: "grok-desktop-guest",
    architecture: "x64",
    sequence: 7,
    images: [{
      id: "grok-guest-1.2.3", version: "1.2.3", stagingName: "grok-guest.vhdx",
      sha256: "a".repeat(64), sizeBytes: 123,
    }],
    signature: { algorithm: "ed25519", keyId: "release-2026", value: "excluded" },
  };
  const signed = guestImageCatalogSigningBytes(catalog).toString("utf8");
  assert.match(signed, /^\{"schemaVersion":1,"product":"grok-desktop-guest"/);
  assert.doesNotMatch(signed, /excluded/);
  assert.ok(signed.endsWith("\n"));
});

test("accepts only public trust and hardware or certificate-store signing policy", () => {
  const environment = releaseEnvironment();
  const parsed = readReleaseEnvironment(environment);
  assert.equal(parsed.signerThumbprint, signerThumbprint);
  assert.equal(parsed.releaseMetadataKeys.size, 1);
  assert.equal(parsed.acpCatalogTrust.binding, trustedAcpKeys.binding);
  assert.equal(parsed.acpProvenanceEvidenceID, "xai-download-attestation-42");
  assert.equal(parsed.acpRedistributionEvidenceID, "xai-redistribution-approval-7");
  assert.throws(() => readReleaseEnvironment({
    ...environment, GROK_WINDOWS_SIGN_ARGS_JSON: '["/f","secret.pfx"]',
  }), /hardware-backed/);
  assert.throws(() => readReleaseEnvironment({
    ...environment, GROK_WINDOWS_TIMESTAMP_SERVER: "http://timestamp.invalid",
  }), /HTTPS/);
  for (const name of ["WINDOWS_CERTIFICATE_FILE", "CSC_LINK", "CSC_KEY_PASSWORD", "WIN_CSC_LINK"]) {
    assert.throws(() => readReleaseEnvironment({ ...environment, [name]: "forbidden" }), /ambient/);
  }
  assert.deepEqual(createSigningToolEnvironment({
    Path: "C:\\untrusted", SystemRoot: "C:\\Windows", WINDOWS_CERTIFICATE_FILE: "secret.pfx",
  }, { GROK_SIGNATURE_FILE: "C:\\release\\Grok.msix" }), {
    SystemRoot: "C:\\Windows", GROK_SIGNATURE_FILE: "C:\\release\\Grok.msix",
  });
});

test("validates signer identity and renders the packaged application manifest", async () => {
  assert.deepEqual(validateSignerIdentity({
    subject: "CN=Grok Desktop Test", thumbprint: signerThumbprint.toLowerCase(),
  }, "CN=Grok Desktop Test", signerThumbprint), {
    subject: "CN=Grok Desktop Test", thumbprint: signerThumbprint,
  });
  assert.throws(() => validateSignerIdentity({
    subject: "CN=Other", thumbprint: signerThumbprint,
  }, "CN=Grok Desktop Test", signerThumbprint), /subject/);
  assert.equal(renderManifest("<x a=\"@@VALUE@@\"/>", { VALUE: "A & B" }), "<x a=\"A &amp; B\"/>");
  const template = await readFile(new URL("../release/windows/AppxManifest.xml.template", import.meta.url), "utf8");
  const manifest = renderManifest(template, {
    PACKAGE_IDENTITY: "GrokDesktop.Test",
    PUBLISHER: "CN=Grok Desktop & Test",
    PACKAGE_VERSION: "1.2.3.0",
    ARCHITECTURE: "x64",
    PUBLISHER_DISPLAY_NAME: "Grok Desktop Test",
    MAX_TESTED_VERSION: "10.0.26100.0",
  });
  const identity = manifest.match(/<Identity\s+([\s\S]*?)\s*\/>/);
  assert.ok(identity);
  assert.match(identity[1], /\bName="GrokDesktop\.Test"/);
  assert.match(identity[1], /\bPublisher="CN=Grok Desktop &amp; Test"/);
  assert.match(identity[1], /\bVersion="1\.2\.3\.0"/);
  assert.match(identity[1], /\bProcessorArchitecture="x64"/);

  const application = manifest.match(
    /<Application\s+Id="GrokDesktop"\s+Executable="app\\Grok Desktop\.exe"\s+EntryPoint="Windows\.FullTrustApplication">([\s\S]*?)<\/Application>/,
  );
  assert.ok(application);
  const protocolExtensions = application[1].match(/<uap3:Extension\b[\s\S]*?<\/uap3:Extension>/g);
  assert.ok(protocolExtensions);
  assert.equal(protocolExtensions.length, 1);
  assert.match(protocolExtensions[0], /\bCategory="windows\.protocol"/);
  assert.match(protocolExtensions[0], /\bExecutable="app\\Grok Desktop\.exe"/);
  assert.match(protocolExtensions[0], /\bEntryPoint="Windows\.FullTrustApplication"/);
  assert.match(protocolExtensions[0], /<uap3:Protocol Name="grok-desktop" Parameters="&quot;%1&quot;" \/>/);
  assert.equal((manifest.match(/<uap3:Protocol\b/g) ?? []).length, 1);
  assert.equal((manifest.match(/Parameters="&quot;%1&quot;"/g) ?? []).length, 1);

  const serviceExtensions = application[1].match(/<desktop6:Extension\b[\s\S]*?<\/desktop6:Extension>/g);
  assert.ok(serviceExtensions);
  assert.equal(serviceExtensions.length, 1);
  assert.match(serviceExtensions[0], /Category="windows\.service"/);
  assert.match(serviceExtensions[0], /StartAccount="localSystem"/);
  assert.doesNotMatch(manifest, /@@/);
});

test("keeps the vendor Grok executable byte-identical during first-party signing", async () => {
  const root = path.resolve("packaged-app");
  const component = path.join(root, "resources", "bin", "components", "grok-acp", "bin", "grok.exe");
  assert.equal(shouldAuthenticodeSignPackagedFile(root, component), false);
  assert.equal(shouldAuthenticodeSignPackagedFile(root, path.join(root, "resources", "bin", "grok-daemon.exe")), true);
  const source = await readFile(new URL("./package-windows.mjs", import.meta.url), "utf8");
  assert.match(source, /shouldAuthenticodeSignPackagedFile/);
  assert.match(source, /await signAndVerifyDirectory[\s\S]*?await verifyPackagedNativeLayout/);
  assert.match(source, /extraResource:[\s\S]*?path\.join\(desktopRoot, "assets", "tray"\)/);
  assert.match(source, /packageMSIX\(\{[\s\S]*?sign: false,[\s\S]*?\}\);\s*await signArtifact\(msixPackage,/);
  assert.match(source, /schemaVersion: 3/);
  assert.doesNotMatch(source, /credentialHelper/);
  assert.match(source, /acpCatalogExpiresAtUnixSeconds/);
  assert.match(source, /preserve-vendor-signature-do-not-resign/);
});

test("inspects PE architecture and embedded daemon/service trust", async (t) => {
  const root = await mkdtemp(path.join(os.tmpdir(), "grok-release-pe-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const component = path.join(root, "component.exe");
  await writeFile(component, portableExecutable(0x8664));
  await inspectPortableExecutable(component, "x64");
  await assert.rejects(inspectPortableExecutable(component, "arm64"), /architecture/);
  const daemon = path.join(root, "grok-daemon.exe");
  await writeFile(daemon, trustedDaemonExecutable(0x8664));
  await inspectDaemonAcpCatalogTrust(daemon, trustedAcpKeys);
  await writeFile(daemon, portableExecutable(0x8664));
  await assert.rejects(inspectDaemonAcpCatalogTrust(daemon, trustedAcpKeys), /approved ACP catalog trust/);
  const service = path.join(root, "grok-vm-service.exe");
  await writeFile(service, trustedServiceExecutable(0x8664));
  await inspectServiceGuestCatalogTrust(service, trustedReleaseKeys);
});

test("independently verifies official Grok catalog trust and strict metadata", async (t) => {
  const root = await mkdtemp(path.join(os.tmpdir(), "grok-acp-catalog-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const catalogPath = path.join(root, "catalog.json");
  const valid = acpPayload();
  await writeFile(catalogPath, signedAcpEnvelope(valid));
  const verified = await verifyOfficialGrokCatalog(catalogPath, "x64", trustedAcpKeys, acpNow);
  assert.equal(verified.component.version, "0.2.95");

  await writeFile(catalogPath, signedAcpEnvelope(valid, generateKeyPairSync("ed25519").privateKey));
  await assert.rejects(
    verifyOfficialGrokCatalog(catalogPath, "x64", trustedAcpKeys, acpNow), /signature is invalid/,
  );
  await writeFile(catalogPath, signedAcpEnvelope(acpPayload({ architecture: "aarch64" })));
  await assert.rejects(
    verifyOfficialGrokCatalog(catalogPath, "x64", trustedAcpKeys, acpNow), /package platform/,
  );
  await writeFile(catalogPath, signedAcpEnvelope(acpPayload({ expiresAtUnixSeconds: acpNow })));
  await assert.rejects(
    verifyOfficialGrokCatalog(catalogPath, "x64", trustedAcpKeys, acpNow), /expired/,
  );
  await writeFile(catalogPath, signedAcpEnvelope(acpPayload({ executable: "../grok.exe" })));
  await assert.rejects(
    verifyOfficialGrokCatalog(catalogPath, "x64", trustedAcpKeys, acpNow), /record is invalid/,
  );
  await writeFile(catalogPath, signedAcpEnvelope(acpPayload({ name: "unofficial" })));
  await assert.rejects(
    verifyOfficialGrokCatalog(catalogPath, "x64", trustedAcpKeys, acpNow), /record is invalid/,
  );

  const nonCanonical = JSON.parse(signedAcpEnvelope(valid).toString("utf8"));
  nonCanonical.payload += "=";
  await writeFile(catalogPath, JSON.stringify(nonCanonical));
  await assert.rejects(
    verifyOfficialGrokCatalog(catalogPath, "x64", trustedAcpKeys, acpNow), /canonical base64/,
  );

  const duplicatePayload = Buffer.from(
    `{"schema":"grok.official-component-catalog/v1","sequence":7,"sequence":8,"expiresAtUnixSeconds":${acpNow + 3600},"components":[]}`,
  );
  await writeFile(catalogPath, signedAcpEnvelope(duplicatePayload));
  await assert.rejects(
    verifyOfficialGrokCatalog(catalogPath, "x64", trustedAcpKeys, acpNow), /strict JSON/,
  );
});

test("validates the complete signed release inventory and native digest bindings", async (t) => {
  const fixture = await createReleaseStage(t);
  const validated = await validateReleaseInputs(fixture.root, fixture.expected);
  assert.equal(validated.files.size, 8);
  assert.equal(validated.acpCatalog.sequence, 7);
  assert.equal(validated.acpComponent.version, "0.2.95");
  assert.equal(validated.acpComponent.stagePath, "bin/components/grok-acp/bin/grok.exe");
  await writeFile(path.join(fixture.root, "bin", "unexpected.exe"), portableExecutable(0x8664));
  await assert.rejects(validateReleaseInputs(fixture.root, fixture.expected), /unexpected file or directory/);

  const newerContractFixture = await createReleaseStage(t);
  newerContractFixture.manifest.version = 4;
  signInputManifest(newerContractFixture.manifest);
  await writeFile(
    path.join(newerContractFixture.root, "release-inputs.json"),
    JSON.stringify(newerContractFixture.manifest),
  );
  await assert.rejects(
    validateReleaseInputs(newerContractFixture.root, newerContractFixture.expected), /unsupported schema/,
  );
});

test("strictly validates the integration release catalog contract", () => {
  const manifestDigest = "a".repeat(64);
  const valid = {
    version: 1,
    revision: 7,
    bundles: [{
      id: "desktop.grok.wisp",
      version: "1.2.3",
      rootIndex: 0,
      bundlePath: "desktop.grok.wisp/1.2.3",
      manifestPath: "manifest.json",
      manifestSha256: manifestDigest,
      allowedCapabilities: ["computer-use.act", "computer-use.observe"],
      files: [{ path: "manifest.json", sha256: manifestDigest, size: 128, executable: false }],
    }],
  };
  assert.equal(
    JSON.stringify(parseIntegrationCatalog(Buffer.from(JSON.stringify(valid)))),
    JSON.stringify(valid),
  );

  assert.throws(
    () => parseIntegrationCatalog(Buffer.from('{"version":1,"revision":1,"revision":2,"bundles":[]}')),
    /strict JSON/,
  );
  assert.throws(
    () => parseIntegrationCatalog(Buffer.from(JSON.stringify({ ...valid, ignored: true }))),
    /header is invalid/,
  );
  assert.throws(
    () => parseIntegrationCatalog(Buffer.from(JSON.stringify({
      ...valid,
      bundles: [{ ...valid.bundles[0], manifestPath: "../manifest.json" }],
    }))),
    /entry is invalid/,
  );
  assert.throws(
    () => parseIntegrationCatalog(Buffer.from(JSON.stringify({
      ...valid,
      bundles: [{ ...valid.bundles[0], manifestSha256: "b".repeat(64) }],
    }))),
    /manifest binding is invalid/,
  );
});

test("rejects outer-signed ACP digest and daemon-trust substitution", async (t) => {
  const digestFixture = await createReleaseStage(t);
  const wrongDigestCatalog = signedAcpCatalog({ sha256: "f".repeat(64) });
  await replaceSignedStageFile(digestFixture, "bin/components/grok-acp/catalog.json", wrongDigestCatalog);
  await assert.rejects(
    validateReleaseInputs(digestFixture.root, digestFixture.expected), /does not match the release inventory/,
  );

  const daemonFixture = await createReleaseStage(t);
  await replaceSignedStageFile(daemonFixture, "bin/grok-daemon.exe", portableExecutable(0x8664));
  await assert.rejects(
    validateReleaseInputs(daemonFixture.root, daemonFixture.expected), /approved ACP catalog trust binding/,
  );

});

test("preserves the exact native runtime layout and unsigned input bytes", async (t) => {
  const fixture = await createReleaseStage(t);
  const inputs = await validateReleaseInputs(fixture.root, fixture.expected);
  const appDirectory = path.join(fixture.root, "packaged", "Grok Desktop-win32-x64");
  await mkdir(path.join(appDirectory, "resources"), { recursive: true });
  await cp(path.join(fixture.root, "bin"), path.join(appDirectory, "resources", "bin"), {
    recursive: true, dereference: false, errorOnExist: true,
  });
  const layout = await verifyPackagedNativeLayout(appDirectory, inputs, "x64");
  assert.equal(path.relative(path.dirname(layout.daemon), layout.catalog).split(path.sep).join("/"),
    "components/grok-acp/catalog.json");

  const daemonBytes = await readFile(layout.daemon);
  await writeFile(layout.daemon, Buffer.concat([daemonBytes, Buffer.from("tampered")]));
  await assert.rejects(verifyPackagedNativeLayout(appDirectory, inputs, "x64"), /bytes differ/);
  await writeFile(layout.daemon, daemonBytes);

  await writeFile(layout.component, Buffer.concat([await readFile(layout.component), Buffer.from("tampered")]));
  await assert.rejects(verifyPackagedNativeLayout(
    appDirectory, inputs, "x64", { firstPartyBinariesSigned: true },
  ), /bytes differ/);
});

function releaseEnvironment() {
  return {
    GROK_MSIX_IDENTITY: "GrokDesktop.Test",
    GROK_MSIX_PUBLISHER: "CN=Grok Desktop Test",
    GROK_MSIX_PUBLISHER_DISPLAY_NAME: "Grok Desktop Test",
    GROK_WINDOWS_MAX_TESTED_VERSION: "10.0.26100.0",
    GROK_WINDOWS_SIGNTOOL_PATH: "C:\\Program Files (x86)\\Windows Kits\\10\\bin\\signtool.exe",
    GROK_WINDOWS_POWERSHELL_PATH: "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe",
    GROK_WINDOWS_TIMESTAMP_SERVER: "https://timestamp.example.test",
    GROK_WINDOWS_SIGNER_SHA1: signerThumbprint,
    GROK_WINDOWS_SIGN_ARGS_JSON: `["/sha1","${signerThumbprint}"]`,
    GROK_RELEASE_METADATA_PUBLIC_KEYS_JSON: JSON.stringify({ [releaseKeyID]: releasePublicKey }),
    GROK_UPDATE_TRUSTED_KEYS_JSON: JSON.stringify({ [releaseKeyID]: releasePublicKey }),
    GROK_ACP_CATALOG_TRUSTED_KEYS: acpTrustRaw,
    GROK_XAI_COMPONENT_PROVENANCE_EVIDENCE_ID: "xai-download-attestation-42",
    GROK_XAI_COMPONENT_REDISTRIBUTION_EVIDENCE_ID: "xai-redistribution-approval-7",
  };
}

async function createReleaseStage(t) {
  const root = await mkdtemp(path.join(os.tmpdir(), "grok-release-stage-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const acpExecutable = portableExecutable(0x8664);
  const files = new Map([
    ["bin/grok-daemon.exe", trustedDaemonExecutable(0x8664)],
    ["bin/components/grok-acp/bin/grok.exe", acpExecutable],
    ["bin/components/grok-acp/catalog.json", signedAcpCatalog({
      sha256: hash(acpExecutable), size: acpExecutable.length,
    })],
    ["service/grok-vm-service.exe", trustedServiceExecutable(0x8664)],
    ["guest/grok-guest.vhdx", Buffer.concat([Buffer.from("vhdxfile"), Buffer.alloc(128)])],
    ["catalog/integrations.json", Buffer.from('{"version":1,"revision":1,"bundles":[]}')],
  ]);
  const guestDigest = hash(files.get("guest/grok-guest.vhdx"));
  files.set("catalog/components.json", signedGuestCatalog(guestDigest, files.get("guest/grok-guest.vhdx").length));
  files.set("guest/grok-guest.vhdx.sha256", Buffer.from(`${guestDigest}  grok-guest.vhdx\n`));
  const records = [];
  for (const [relative, contents] of [...files].toSorted(([left], [right]) => left.localeCompare(right))) {
    const destination = path.join(root, ...relative.split("/"));
    await mkdir(path.dirname(destination), { recursive: true });
    await writeFile(destination, contents);
    records.push({ path: relative, sha256: hash(contents), size: contents.length });
  }
  const manifest = signedInputManifest(records, files.get("guest/grok-guest.vhdx").length, guestDigest);
  await writeFile(path.join(root, "release-inputs.json"), JSON.stringify(manifest));
  const environment = readReleaseEnvironment(releaseEnvironment());
  return {
    root,
    files,
    manifest,
    expected: {
      architecture: "x64",
      channel: "stable",
      desktopVersion: "0.1.0",
      releaseMetadataKeys: environment.releaseMetadataKeys,
      acpCatalogTrust: environment.acpCatalogTrust,
      nowUnixSeconds: acpNow,
    },
  };
}

function signedInputManifest(files, guestSize, guestSHA256) {
  const manifest = {
    version: 3,
    product: "grok-desktop",
    architecture: "x64",
    channel: "stable",
    desktopVersion: "0.1.0",
    sequence: 1,
    guest: {
      imageId: "grok-guest-1.0.0", imageVersion: "1.0.0", stagingName: "grok-guest.vhdx",
      path: "guest/grok-guest.vhdx", sha256: guestSHA256, size: guestSize,
    },
    files,
    signature: { algorithm: "ed25519", keyId: releaseKeyID, value: "" },
  };
  signInputManifest(manifest);
  return manifest;
}

function signInputManifest(manifest) {
  manifest.signature.value = signData(
    null, releaseInputSigningBytes(manifest), releaseKeys.privateKey,
  ).toString("base64");
}

function signedGuestCatalog(sha256, size) {
  const catalog = {
    schemaVersion: 1,
    product: "grok-desktop-guest",
    architecture: "x64",
    sequence: 1,
    images: [{
      id: "grok-guest-1.0.0", version: "1.0.0", stagingName: "grok-guest.vhdx",
      sha256, sizeBytes: size,
    }],
    signature: { algorithm: "ed25519", keyId: releaseKeyID, value: "" },
  };
  catalog.signature.value = signData(
    null, guestImageCatalogSigningBytes(catalog), releaseKeys.privateKey,
  ).toString("base64");
  return Buffer.from(`${JSON.stringify(catalog)}\n`, "utf8");
}

function acpPayload(overrides = {}) {
  const component = {
    name: overrides.name ?? "grok-build",
    publisher: "xAI",
    version: "0.2.95",
    os: "windows",
    architecture: overrides.architecture ?? "x86_64",
    executable: overrides.executable ?? "bin/grok.exe",
    sha256: overrides.sha256 ?? hash(portableExecutable(0x8664)),
    size: overrides.size ?? portableExecutable(0x8664).length,
  };
  return {
    schema: "grok.official-component-catalog/v1",
    sequence: 7,
    expiresAtUnixSeconds: overrides.expiresAtUnixSeconds ?? acpNow + 3600,
    components: [component],
  };
}

function signedAcpCatalog(overrides = {}) {
  return signedAcpEnvelope(acpPayload(overrides));
}

function signedAcpEnvelope(payload, privateKey = acpKeys.privateKey, keyID = acpKeyID) {
  const payloadBytes = Buffer.isBuffer(payload) ? payload : Buffer.from(JSON.stringify(payload), "utf8");
  const signature = signData(
    null, officialGrokCatalogSignatureBytes(keyID, payloadBytes), privateKey,
  ).toString("base64");
  return Buffer.from(JSON.stringify({
    schema: "grok.official-component-catalog-envelope/v1",
    keyId: keyID,
    payload: payloadBytes.toString("base64"),
    signature,
  }), "utf8");
}

function trustedServiceExecutable(machine) {
  const trust = serviceGuestCatalogTrust(trustedReleaseKeys);
  return Buffer.concat([portableExecutable(machine), Buffer.from(`\0${trust.encoded}\0${trust.binding}\0`)]);
}

function trustedDaemonExecutable(machine) {
  return Buffer.concat([
    portableExecutable(machine), Buffer.from(`\0${trustedAcpKeys.raw}\0${trustedAcpKeys.binding}\0`),
  ]);
}

async function replaceSignedStageFile(fixture, relative, contents) {
  await writeFile(path.join(fixture.root, ...relative.split("/")), contents);
  const record = fixture.manifest.files.find((candidate) => candidate.path === relative);
  record.sha256 = hash(contents);
  record.size = contents.length;
  signInputManifest(fixture.manifest);
  await writeFile(path.join(fixture.root, "release-inputs.json"), JSON.stringify(fixture.manifest));
}

function portableExecutable(machine) {
  const output = Buffer.alloc(256);
  output.write("MZ", 0, "ascii");
  output.writeUInt32LE(128, 0x3c);
  output.write("PE\0\0", 128, "binary");
  output.writeUInt16LE(machine, 132);
  return output;
}

function hash(value) {
  return createHash("sha256").update(value).digest("hex");
}

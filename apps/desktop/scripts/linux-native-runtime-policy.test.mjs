import assert from "node:assert/strict";
import { mkdtemp, rm, symlink, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { inspectPortableLinuxRuntimeFile } from "./linux-native-runtime-policy.mjs";

const ET_EXEC = 2;
const ET_DYN = 3;
const PT_LOAD = 1;
const PT_DYNAMIC = 2;
const PF_R = 4;
const PF_X = 1;
const DT_FLAGS_1 = 0x6fff_fffbn;
const DF_1_PIE = 0x0800_0000n;

function elfFixture({
  architecture = "x64",
  executableType = ET_EXEC,
  programTypes = [PT_LOAD],
  dynamicEntries = [],
  dynamicTerminated = true,
  entryPoint,
  loadOffset = 0n,
  loadVirtualAddress,
  loadFileSize,
  loadMemorySize,
  loadAlignment = 0x1000n,
  loadFlags = PF_R | PF_X,
  dynamicOffset,
  dynamicVirtualAddress,
  dynamicFileSize,
  dynamicMemorySize,
  trailingBytes = 0,
} = {}) {
  const programHeadersSize = 56 * programTypes.length;
  const defaultDynamicOffset = 64 + programHeadersSize;
  const dynamicSlots = dynamicEntries.length + (dynamicTerminated ? 1 : 0);
  const defaultDynamicSize = programTypes.includes(PT_DYNAMIC) ? 16 * dynamicSlots : 0;
  const minimumSize = Math.max(0x200, defaultDynamicOffset + defaultDynamicSize + trailingBytes);
  const bytes = Buffer.alloc(minimumSize);
  const defaultLoadVirtualAddress = executableType === ET_DYN ? 0n : 0x40_0000n;
  const resolvedLoadVirtualAddress = loadVirtualAddress ?? defaultLoadVirtualAddress;
  const resolvedEntryPoint = entryPoint ?? (resolvedLoadVirtualAddress + 0x100n);
  const resolvedLoadFileSize = loadFileSize ?? (BigInt(bytes.length) - loadOffset);
  const resolvedLoadMemorySize = loadMemorySize ?? resolvedLoadFileSize;
  const resolvedDynamicOffset = dynamicOffset ?? BigInt(defaultDynamicOffset);
  const resolvedDynamicVirtualAddress = dynamicVirtualAddress
    ?? (resolvedLoadVirtualAddress + (resolvedDynamicOffset - loadOffset));
  const resolvedDynamicFileSize = dynamicFileSize ?? BigInt(defaultDynamicSize);
  const resolvedDynamicMemorySize = dynamicMemorySize ?? resolvedDynamicFileSize;

  bytes.set([0x7f, 0x45, 0x4c, 0x46, 2, 1, 1], 0);
  bytes.writeUInt16LE(executableType, 16);
  bytes.writeUInt16LE(architecture === "x64" ? 62 : 183, 18);
  bytes.writeUInt32LE(1, 20);
  bytes.writeBigUInt64LE(resolvedEntryPoint, 24);
  bytes.writeBigUInt64LE(64n, 32);
  bytes.writeUInt16LE(64, 52);
  bytes.writeUInt16LE(56, 54);
  bytes.writeUInt16LE(programTypes.length, 56);
  for (const [index, type] of programTypes.entries()) {
    const entryOffset = 64 + (index * 56);
    bytes.writeUInt32LE(type, entryOffset);
    if (type === PT_LOAD) {
      bytes.writeUInt32LE(loadFlags, entryOffset + 4);
      bytes.writeBigUInt64LE(loadOffset, entryOffset + 8);
      bytes.writeBigUInt64LE(resolvedLoadVirtualAddress, entryOffset + 16);
      bytes.writeBigUInt64LE(resolvedLoadVirtualAddress, entryOffset + 24);
      bytes.writeBigUInt64LE(resolvedLoadFileSize, entryOffset + 32);
      bytes.writeBigUInt64LE(resolvedLoadMemorySize, entryOffset + 40);
      bytes.writeBigUInt64LE(loadAlignment, entryOffset + 48);
    }
    if (type === PT_DYNAMIC) {
      bytes.writeUInt32LE(PF_R, entryOffset + 4);
      bytes.writeBigUInt64LE(resolvedDynamicOffset, entryOffset + 8);
      bytes.writeBigUInt64LE(resolvedDynamicVirtualAddress, entryOffset + 16);
      bytes.writeBigUInt64LE(resolvedDynamicVirtualAddress, entryOffset + 24);
      bytes.writeBigUInt64LE(resolvedDynamicFileSize, entryOffset + 32);
      bytes.writeBigUInt64LE(resolvedDynamicMemorySize, entryOffset + 40);
      bytes.writeBigUInt64LE(8n, entryOffset + 48);
    }
  }
  for (const [index, entry] of dynamicEntries.entries()) {
    const tableOffset = Number(resolvedDynamicOffset) + (index * 16);
    const { tag, value = 0n } = typeof entry === "object" ? entry : { tag: entry };
    bytes.writeBigInt64LE(BigInt(tag), tableOffset);
    bytes.writeBigUInt64LE(BigInt(value), tableOffset + 8);
  }
  return bytes;
}

async function executableFixture(t, bytes) {
  const root = await mkdtemp(path.join(os.tmpdir(), "grok-linux-runtime-policy-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const executable = path.join(root, "runtime");
  await writeFile(executable, bytes, { mode: 0o755 });
  return { executable, root };
}

test("accepts a structurally valid self-contained static Linux ELF runtime", async (t) => {
  const { executable } = await executableFixture(t, elfFixture({ programTypes: [PT_LOAD, 4] }));
  const result = await inspectPortableLinuxRuntimeFile(executable, "x64", "test runtime");
  assert.deepEqual(result, { architecture: "x64", linkage: "static", loadSegments: 1 });
});

test("accepts dependency-free static PIE dynamic relocations", async (t) => {
  const { executable } = await executableFixture(
    t,
    elfFixture({
      executableType: ET_DYN,
      programTypes: [PT_LOAD, PT_DYNAMIC],
      dynamicEntries: [
        { tag: 7n },
        { tag: DT_FLAGS_1, value: DF_1_PIE },
      ],
    }),
  );
  const result = await inspectPortableLinuxRuntimeFile(executable, "x64", "test runtime");
  assert.deepEqual(result, { architecture: "x64", linkage: "static-pie", loadSegments: 1 });
});

test("rejects missing or non-executable entry points", async (t) => {
  for (const [name, options] of [
    ["zero entry", { entryPoint: 0n }],
    ["entry outside LOAD", { entryPoint: 0x50_0000n }],
    ["entry in non-executable LOAD", { loadFlags: PF_R }],
  ]) {
    await t.test(name, async (subtest) => {
      const { executable } = await executableFixture(subtest, elfFixture(options));
      await assert.rejects(
        inspectPortableLinuxRuntimeFile(executable, "x64", "test runtime"),
        /entry point is not in a file-backed executable PT_LOAD/,
      );
    });
  }
});

test("rejects malformed PT_LOAD mappings", async (t) => {
  for (const [name, options, pattern] of [
    ["filesz exceeds memsz", { loadFileSize: 0x200n, loadMemorySize: 0x100n }, /invalid ELF PT_LOAD mapping/],
    ["file range exceeds input", { loadFileSize: 0x201n }, /invalid ELF PT_LOAD mapping/],
    ["non-power-of-two alignment", { loadAlignment: 3n }, /invalid ELF PT_LOAD mapping/],
    [
      "incongruent virtual and file offsets",
      { loadOffset: 1n, loadFileSize: 0x1ffn, loadMemorySize: 0x1ffn },
      /invalid ELF PT_LOAD mapping/,
    ],
    [
      "virtual address overflow",
      {
        entryPoint: 0xffff_ffff_ffff_fffcn,
        loadVirtualAddress: 0xffff_ffff_ffff_fff8n,
        loadFileSize: 8n,
        loadMemorySize: 16n,
        loadAlignment: 1n,
      },
      /address range overflows ELF64/,
    ],
    [
      "file offset overflow",
      {
        loadOffset: 0xffff_ffff_ffff_fff8n,
        loadFileSize: 16n,
        loadMemorySize: 16n,
        loadAlignment: 1n,
      },
      /address range overflows ELF64/,
    ],
  ]) {
    await t.test(name, async (subtest) => {
      const { executable } = await executableFixture(subtest, elfFixture(options));
      await assert.rejects(
        inspectPortableLinuxRuntimeFile(executable, "x64", "test runtime"),
        pattern,
      );
    });
  }
});

test("rejects every dynamic loader and dependency-control tag", async (t) => {
  for (const [name, tag] of [
    ["DT_NEEDED", 1n],
    ["DT_RPATH", 15n],
    ["DT_RUNPATH", 29n],
    ["DT_CONFIG", 0x6fff_fefan],
    ["DT_DEPAUDIT", 0x6fff_fefbn],
    ["DT_AUDIT", 0x6fff_fefcn],
    ["DT_AUXILIARY", 0x7fff_fffdn],
    ["DT_FILTER", 0x7fff_ffffn],
  ]) {
    await t.test(name, async (subtest) => {
      const { executable } = await executableFixture(
        subtest,
        elfFixture({ programTypes: [PT_LOAD, PT_DYNAMIC], dynamicEntries: [{ tag }] }),
      );
      await assert.rejects(
        inspectPortableLinuxRuntimeFile(executable, "x64", "test runtime"),
        new RegExp(`${name} references host libraries`),
      );
    });
  }

  await t.test("PT_INTERP", async (subtest) => {
    const { executable } = await executableFixture(
      subtest,
      elfFixture({ programTypes: [PT_LOAD, 3] }),
    );
    await assert.rejects(
      inspectPortableLinuxRuntimeFile(executable, "x64", "test runtime"),
      /PT_INTERP requires a host loader/,
    );
  });
});

test("rejects shared objects and ambiguous static-PIE metadata", async (t) => {
  for (const [name, options, pattern] of [
    [
      "ET_EXEC with PT_DYNAMIC",
      { programTypes: [PT_LOAD, PT_DYNAMIC], dynamicEntries: [{ tag: 7n }] },
      /ET_EXEC executable has unexpected PT_DYNAMIC/,
    ],
    [
      "ET_DYN without PT_DYNAMIC",
      { executableType: ET_DYN },
      /ET_DYN executable is missing DF_1_PIE/,
    ],
    [
      "ET_DYN without DT_FLAGS_1",
      { executableType: ET_DYN, programTypes: [PT_LOAD, PT_DYNAMIC], dynamicEntries: [{ tag: 7n }] },
      /ET_DYN executable is missing DF_1_PIE/,
    ],
    [
      "ET_DYN without DF_1_PIE flag",
      {
        executableType: ET_DYN,
        programTypes: [PT_LOAD, PT_DYNAMIC],
        dynamicEntries: [{ tag: DT_FLAGS_1, value: 1n }],
      },
      /ET_DYN executable is missing DF_1_PIE/,
    ],
    [
      "duplicate DT_FLAGS_1",
      {
        executableType: ET_DYN,
        programTypes: [PT_LOAD, PT_DYNAMIC],
        dynamicEntries: [
          { tag: DT_FLAGS_1, value: DF_1_PIE },
          { tag: DT_FLAGS_1, value: DF_1_PIE },
        ],
      },
      /ambiguous ELF DT_FLAGS_1 metadata/,
    ],
    [
      "duplicate PT_DYNAMIC",
      {
        executableType: ET_DYN,
        programTypes: [PT_LOAD, PT_DYNAMIC, PT_DYNAMIC],
        dynamicEntries: [{ tag: DT_FLAGS_1, value: DF_1_PIE }],
      },
      /ambiguous ELF dynamic metadata/,
    ],
    [
      "unterminated dynamic table",
      {
        programTypes: [PT_LOAD, PT_DYNAMIC],
        dynamicEntries: [{ tag: 7n }],
        dynamicTerminated: false,
      },
      /dynamic metadata is unterminated/,
    ],
  ]) {
    await t.test(name, async (subtest) => {
      const { executable } = await executableFixture(subtest, elfFixture(options));
      await assert.rejects(
        inspectPortableLinuxRuntimeFile(executable, "x64", "test runtime"),
        pattern,
      );
    });
  }
});

test("rejects malformed dynamic table bounds", async (t) => {
  for (const [name, options] of [
    ["smaller than one entry", { dynamicFileSize: 8n, dynamicMemorySize: 8n }],
    ["partial entry", { dynamicFileSize: 24n, dynamicMemorySize: 24n }],
    ["filesz exceeds memsz", { dynamicFileSize: 32n, dynamicMemorySize: 16n }],
    ["oversized table", { dynamicFileSize: 0x10_0010n, dynamicMemorySize: 0x10_0010n }],
  ]) {
    await t.test(name, async (subtest) => {
      const { executable } = await executableFixture(
        subtest,
        elfFixture({
          programTypes: [PT_LOAD, PT_DYNAMIC],
          dynamicEntries: [{ tag: 7n }],
          ...options,
        }),
      );
      await assert.rejects(
        inspectPortableLinuxRuntimeFile(executable, "x64", "test runtime"),
        /dynamic metadata is out of bounds/,
      );
    });
  }
});

test("rejects PT_DYNAMIC metadata that is not genuinely LOAD-mapped", async (t) => {
  for (const [name, options] of [
    [
      "dynamic file range outside LOAD",
      {
        entryPoint: 0x40_0080n,
        programTypes: [PT_LOAD, PT_DYNAMIC],
        dynamicEntries: [{ tag: 7n }],
        loadFileSize: 176n,
        loadMemorySize: 176n,
      },
    ],
    [
      "dynamic virtual-to-file mapping mismatch",
      {
        programTypes: [PT_LOAD, PT_DYNAMIC],
        dynamicEntries: [{ tag: 7n }],
        dynamicVirtualAddress: 0x40_00b8n,
      },
    ],
  ]) {
    await t.test(name, async (subtest) => {
      const { executable } = await executableFixture(subtest, elfFixture(options));
      await assert.rejects(
        inspectPortableLinuxRuntimeFile(executable, "x64", "test runtime"),
        /PT_DYNAMIC is not consistently mapped by a PT_LOAD/,
      );
    });
  }
});

test("rejects wrong-architecture, malformed, and linked runtime inputs", async (t) => {
  const { executable, root } = await executableFixture(t, elfFixture({ architecture: "arm64" }));
  await assert.rejects(
    inspectPortableLinuxRuntimeFile(executable, "x64", "test runtime"),
    /expected Linux ELF64 executable/,
  );

  const linked = path.join(root, "linked-runtime");
  await symlink(executable, linked);
  await assert.rejects(
    inspectPortableLinuxRuntimeFile(linked, "arm64", "test runtime"),
    /symbolic link|ELOOP/,
  );

  for (const [name, offset, write] of [
    ["ELF version", 20, (bytes) => bytes.writeUInt32LE(2, 20)],
    ["OS ABI", 7, (bytes) => bytes.writeUInt8(9, 7)],
    ["ABI version", 8, (bytes) => bytes.writeUInt8(1, 8)],
  ]) {
    await t.test(name, async (subtest) => {
      const bytes = elfFixture();
      write(bytes, offset);
      const malformed = await executableFixture(subtest, bytes);
      await assert.rejects(
        inspectPortableLinuxRuntimeFile(malformed.executable, "x64", "test runtime"),
        /expected Linux ELF64 executable/,
      );
    });
  }
});

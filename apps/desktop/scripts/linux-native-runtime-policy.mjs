import { constants as fsConstants } from "node:fs";
import { open } from "node:fs/promises";

const ELF_HEADER_SIZE = 64;
const ELF64_PROGRAM_HEADER_SIZE = 56;
const MAX_PROGRAM_HEADERS = 128;
const MAX_DYNAMIC_TABLE_SIZE = 1024 * 1024;
const MAX_U64 = 0xffff_ffff_ffff_ffffn;
const PT_LOAD = 1;
const PT_DYNAMIC = 2;
const PT_INTERP = 3;
const PF_X = 1;
const DT_NULL = 0n;
const DT_FLAGS_1 = 0x6fff_fffbn;
const DF_1_PIE = 0x0800_0000n;
const FORBIDDEN_DYNAMIC_TAGS = new Map([
  [1n, "DT_NEEDED"],
  [15n, "DT_RPATH"],
  [29n, "DT_RUNPATH"],
  [0x6fff_fefan, "DT_CONFIG"],
  [0x6fff_fefbn, "DT_DEPAUDIT"],
  [0x6fff_fefcn, "DT_AUDIT"],
  [0x7fff_fffdn, "DT_AUXILIARY"],
  [0x7fff_ffffn, "DT_FILTER"],
]);
const OPEN_FLAGS = fsConstants.O_RDONLY
  | (fsConstants.O_CLOEXEC ?? 0)
  | (fsConstants.O_NOFOLLOW ?? 0);

function expectedMachine(architecture) {
  if (architecture === "x64") return 62;
  if (architecture === "arm64") return 183;
  throw new Error("portable Linux runtime architecture is unsupported");
}

async function readExactly(handle, length, position, label) {
  const bytes = Buffer.alloc(length);
  let offset = 0;
  while (offset < length) {
    const result = await handle.read(bytes, offset, length - offset, position + offset);
    if (result.bytesRead === 0) throw new Error(`${label} ELF metadata is truncated`);
    offset += result.bytesRead;
  }
  return bytes;
}

function checkedEnd(start, size, label) {
  const end = start + size;
  if (end > MAX_U64) throw new Error(`${label} address range overflows ELF64`);
  return end;
}

function validSegmentAlignment(alignment) {
  return alignment === 0n
    || alignment === 1n
    || (alignment & (alignment - 1n)) === 0n;
}

/**
 * Verifies that an ELF64 runtime has no userspace interpreter or dynamic
 * dependencies. A dependency-free PT_DYNAMIC is valid for static PIE
 * relocations and preserves ASLR without coupling the executable to host
 * libraries.
 */
export async function inspectPortableLinuxRuntimeHandle(handle, architecture, label) {
  const identityBefore = await handle.stat({ bigint: true });
  if (!identityBefore.isFile() || identityBefore.size < BigInt(ELF_HEADER_SIZE)) {
    throw new Error(`${label} is not a bounded regular ELF executable`);
  }

  const header = await readExactly(handle, ELF_HEADER_SIZE, 0, label);
  const executableType = header.readUInt16LE(16);
  const entryPoint = header.readBigUInt64LE(24);
  if (!header.subarray(0, 4).equals(Buffer.from([0x7f, 0x45, 0x4c, 0x46]))
      || header[4] !== 2
      || header[5] !== 1
      || header[6] !== 1
      || (header[7] !== 0 && header[7] !== 3)
      || header[8] !== 0
      || (executableType !== 2 && executableType !== 3)
      || header.readUInt16LE(18) !== expectedMachine(architecture)
      || header.readUInt32LE(20) !== 1
      || header.readUInt16LE(52) !== ELF_HEADER_SIZE) {
    throw new Error(`${label} is not the expected Linux ELF64 executable`);
  }

  const programHeaderOffset = header.readBigUInt64LE(32);
  const programHeaderEntrySize = header.readUInt16LE(54);
  const programHeaderCount = header.readUInt16LE(56);
  if (programHeaderOffset < BigInt(ELF_HEADER_SIZE)
      || programHeaderEntrySize !== ELF64_PROGRAM_HEADER_SIZE
      || programHeaderCount < 1
      || programHeaderCount > MAX_PROGRAM_HEADERS) {
    throw new Error(`${label} has unsupported ELF program headers`);
  }
  const programHeaderBytes = programHeaderEntrySize * programHeaderCount;
  const programHeaderEnd = programHeaderOffset + BigInt(programHeaderBytes);
  if (programHeaderEnd > identityBefore.size || programHeaderOffset > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw new Error(`${label} ELF program headers are out of bounds`);
  }

  const table = await readExactly(
    handle,
    programHeaderBytes,
    Number(programHeaderOffset),
    label,
  );
  const loadSegments = [];
  const dynamicSegments = [];
  for (let index = 0; index < programHeaderCount; index += 1) {
    const entryOffset = index * programHeaderEntrySize;
    const type = table.readUInt32LE(entryOffset);
    const flags = table.readUInt32LE(entryOffset + 4);
    const offset = table.readBigUInt64LE(entryOffset + 8);
    const virtualAddress = table.readBigUInt64LE(entryOffset + 16);
    const fileSize = table.readBigUInt64LE(entryOffset + 32);
    const memorySize = table.readBigUInt64LE(entryOffset + 40);
    const alignment = table.readBigUInt64LE(entryOffset + 48);
    if (type === PT_LOAD) {
      const fileEnd = checkedEnd(offset, fileSize, `${label} PT_LOAD`);
      const memoryEnd = checkedEnd(virtualAddress, memorySize, `${label} PT_LOAD`);
      if (memorySize === 0n
          || fileSize > memorySize
          || fileEnd > identityBefore.size
          || !validSegmentAlignment(alignment)
          || (alignment > 1n && virtualAddress % alignment !== offset % alignment)) {
        throw new Error(`${label} has an invalid ELF PT_LOAD mapping`);
      }
      loadSegments.push({
        alignment,
        fileEnd,
        fileSize,
        flags,
        memoryEnd,
        memorySize,
        offset,
        virtualAddress,
      });
    }
    if (type === PT_INTERP) {
      throw new Error(`${label} is not portable: ELF PT_INTERP requires a host loader`);
    }
    if (type === PT_DYNAMIC) {
      dynamicSegments.push({
        fileSize,
        memorySize,
        offset,
        virtualAddress,
      });
    }
  }
  if (loadSegments.length < 1) throw new Error(`${label} has no loadable ELF segment`);
  if (entryPoint === 0n || !loadSegments.some((segment) => {
    const fileBackedEntryEnd = segment.virtualAddress + segment.fileSize;
    return (segment.flags & PF_X) !== 0
      && entryPoint >= segment.virtualAddress
      && entryPoint < fileBackedEntryEnd;
  })) {
    throw new Error(`${label} entry point is not in a file-backed executable PT_LOAD`);
  }
  if (dynamicSegments.length > 1) throw new Error(`${label} has ambiguous ELF dynamic metadata`);

  for (const dynamic of dynamicSegments) {
    const dynamicFileEnd = checkedEnd(dynamic.offset, dynamic.fileSize, `${label} PT_DYNAMIC`);
    const dynamicMemoryEnd = checkedEnd(
      dynamic.virtualAddress,
      dynamic.memorySize,
      `${label} PT_DYNAMIC`,
    );
    if (dynamic.fileSize < 16n
        || dynamic.fileSize > BigInt(MAX_DYNAMIC_TABLE_SIZE)
        || dynamic.fileSize % 16n !== 0n
        || dynamic.fileSize > dynamic.memorySize
        || dynamic.offset > BigInt(Number.MAX_SAFE_INTEGER)
        || dynamicFileEnd > identityBefore.size) {
      throw new Error(`${label} ELF dynamic metadata is out of bounds`);
    }
    const mappedLoad = loadSegments.find((segment) => dynamic.offset >= segment.offset
      && dynamicFileEnd <= segment.fileEnd
      && dynamic.virtualAddress >= segment.virtualAddress
      && dynamicMemoryEnd <= segment.memoryEnd
      && dynamic.offset - segment.offset === dynamic.virtualAddress - segment.virtualAddress);
    if (!mappedLoad) {
      throw new Error(`${label} ELF PT_DYNAMIC is not consistently mapped by a PT_LOAD`);
    }
    const dynamicTable = await readExactly(
      handle,
      Number(dynamic.fileSize),
      Number(dynamic.offset),
      label,
    );
    let terminated = false;
    let flags1;
    for (let offset = 0; offset < dynamicTable.length; offset += 16) {
      const tag = dynamicTable.readBigInt64LE(offset);
      if (tag === DT_NULL) {
        terminated = true;
        break;
      }
      const forbiddenName = FORBIDDEN_DYNAMIC_TAGS.get(tag);
      if (forbiddenName) {
        throw new Error(`${label} is not portable: ELF ${forbiddenName} references host libraries`);
      }
      if (tag === DT_FLAGS_1) {
        if (flags1 !== undefined) throw new Error(`${label} has ambiguous ELF DT_FLAGS_1 metadata`);
        flags1 = dynamicTable.readBigUInt64LE(offset + 8);
      }
    }
    if (!terminated) throw new Error(`${label} ELF dynamic metadata is unterminated`);
    if (executableType === 3 && (flags1 === undefined || (flags1 & DF_1_PIE) === 0n)) {
      throw new Error(`${label} ELF ET_DYN executable is missing DF_1_PIE`);
    }
  }
  if (executableType === 3 && dynamicSegments.length === 0) {
    throw new Error(`${label} ELF ET_DYN executable is missing DF_1_PIE`);
  }
  if (executableType === 2 && dynamicSegments.length !== 0) {
    throw new Error(`${label} ELF ET_EXEC executable has unexpected PT_DYNAMIC`);
  }

  const identityAfter = await handle.stat({ bigint: true });
  for (const field of ["dev", "ino", "size", "mode", "mtimeNs", "ctimeNs"]) {
    if (identityAfter[field] !== identityBefore[field]) {
      throw new Error(`${label} changed while its ELF metadata was inspected`);
    }
  }
  return Object.freeze({
    architecture,
    linkage: executableType === 3 ? "static-pie" : "static",
    loadSegments: loadSegments.length,
  });
}

export async function inspectPortableLinuxRuntimeFile(
  filePath,
  architecture,
  label = "portable Linux runtime",
) {
  let handle;
  try {
    handle = await open(filePath, OPEN_FLAGS);
    const identity = await handle.stat({ bigint: true });
    if (!identity.isFile() || (identity.mode & 0o111n) === 0n) {
      throw new Error(`${label} is not an executable regular file`);
    }
    return await inspectPortableLinuxRuntimeHandle(handle, architecture, label);
  } catch (error) {
    if (error?.code === "ELOOP") {
      throw new Error(`${label} must not be a symbolic link`, { cause: error });
    }
    throw error;
  } finally {
    await handle?.close();
  }
}

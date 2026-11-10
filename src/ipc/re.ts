// Reverse-engineering commands (RE-01…09, ANL-12…14, SRCH-12).
import { invoke } from "@tauri-apps/api/core";
import type { EditResult } from "./api";

export interface ArchInfo {
  arch: string;
  label: string;
  canAssemble: boolean;
  hasNop: boolean;
}
export interface ExeRegion {
  name: string;
  offset: number;
  size: number;
}
export interface Section {
  name: string;
  virtualAddress: number;
  virtualSize: number;
  offset: number;
  fileSize: number;
  permissions: string;
  executable: boolean;
}
export interface Sym {
  name: string;
  demangled: string | null;
  address: number | null;
  offset: number | null;
  library?: string | null;
  kind?: string;
}
export interface ExecutableInfo {
  format: "pe" | "elf" | "machO" | "machOFat";
  bits: number;
  endian: string;
  machine: string;
  arch: string | null;
  kind: string;
  entryPoint: number | null;
  entryOffset: number | null;
  headers: ExeRegion[];
  sections: Section[];
  segments: Section[];
  imports: Sym[];
  exports: Sym[];
  symbols: Sym[];
  libraries: string[];
  interpreter: string | null;
  checksum: { offset: number; size: number; value: number } | null;
  slices: { archName: string; offset: number; size: number; error: string | null }[];
  warnings: string[];
}
export interface Finding {
  name: string;
  category: string;
  version: string | null;
  confidence: string;
  evidence: { offset: number; length: number; description: string }[];
}
export interface Instruction {
  address: number;
  size: number;
  bytes: number[];
  mnemonic: string;
  operands: string;
  groups: string[];
  branchTarget: number | null;
  memoryReference: number | null;
  isData: boolean;
}
export interface InstructionPatch {
  address: number;
  original: number[];
  replacement: number[];
  assembledLen: number;
  originalInstructionLen: number;
  padding: number;
  overwritten: Instruction[];
  warning?: string | null;
}
export interface XrefReport {
  target: number;
  code: { from: number; fromOffset: number | null; kind: string; target: number; text: string }[];
  pointers: {
    location: { offset: number; width: number; endian: string };
    virtualAddress: number | null;
    section: string | null;
    isRva: boolean;
  }[];
}
export interface PatchList {
  version: number;
  target: string | null;
  targetSize: number | null;
  patches: { offset: number; original: string; replacement: string; comment: string }[];
}
export interface PortReport {
  results: {
    index: number;
    oldOffset: number;
    status: string;
    newOffset: number | null;
    candidates: number[];
  }[];
  ported: PatchList;
}
export interface ExternalTool {
  id: string;
  name: string;
  launch: "command" | "url";
  note: string;
}

export const re = {
  info: (id: number) =>
    invoke<{ info: ExecutableInfo | null; error: string | null; findings: Finding[] }>("re_info", {
      id,
    }),
  arches: () => invoke<ArchInfo[]>("re_arches"),
  disasm: (id: number, offset: number, len: number, address: number, arch: string, max = 400) =>
    invoke<Instruction[]>("re_disasm", { id, offset, len, address, arch, max }),
  assemble: (arch: string, source: string, address: number) =>
    invoke<number[]>("re_assemble", { arch, source, address }),
  patchPlan: (id: number, offset: number, address: number, arch: string, source: string) =>
    invoke<InstructionPatch>("re_patch_plan", { id, offset, address, arch, source }),
  xrefs: (id: number, va: number) => invoke<XrefReport>("re_xrefs", { id, va, max: 2000 }),
  fixChecksum: (id: number) => invoke<EditResult>("re_fix_checksum", { id }),
  patchesExport: (id: number) => invoke<PatchList>("re_patches_export", { id }),
  patchesCheck: (id: number, list: PatchList) => invoke<string[]>("re_patches_check", { id, list }),
  patchesApply: (id: number, list: PatchList, revert: boolean) =>
    invoke<EditResult>("re_patches_apply", { id, list, revert }),
  patchesPort: (old: number, neu: number, list: PatchList) =>
    invoke<PortReport>("re_patches_port", { old, new: neu, list }),
  tools: () => invoke<ExternalTool[]>("re_tools"),
  toolPrepare: (id: number, start: number, end: number, tool: string, toolDir: string) =>
    invoke<{ launch: "command" | "url"; program: string; args: string[] }>("re_tool_prepare", {
      id,
      start,
      end,
      tool,
      toolDir,
    }),
  toolSpawn: (program: string, args: string[]) => invoke<void>("re_tool_spawn", { program, args }),
};

/** File offset → virtual address through the section table. */
export function offsetToVa(info: ExecutableInfo | null, off: number): number | null {
  if (!info) return null;
  for (const s of info.sections)
    if (s.fileSize > 0 && off >= s.offset && off < s.offset + s.fileSize)
      return s.virtualAddress + (off - s.offset);
  return null;
}

/** Virtual address → file offset, or null when the address has no file bytes. */
export function vaToOffset(info: ExecutableInfo | null, va: number): number | null {
  if (!info) return null;
  for (const s of info.sections)
    if (
      va >= s.virtualAddress &&
      va < s.virtualAddress + Math.min(s.fileSize, s.virtualSize || s.fileSize)
    )
      return s.offset + (va - s.virtualAddress);
  return null;
}

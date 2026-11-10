// Typed wrappers for disks, images, archives, remote sources and process
// memory (src-tauri/src/commands/{sources,memory}.rs).
import { Channel, invoke } from "@tauri-apps/api/core";
import type { CmdError, DocInfo } from "./api";

// ---------------------------------------------------------------- jobs

export type JobEvent<T = unknown> =
  | { type: "progress"; done: number; total: number }
  | { type: "done"; result: T }
  | { type: "error"; error: CmdError };

/** Starts a job command; resolves with its result, rejects with its error. */
export function runJob<T>(
  cmd: string,
  args: Record<string, unknown>,
  onProgress?: (done: number, total: number) => void,
  onJob?: (job: number) => void,
): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const channel = new Channel<JobEvent<T>>();
    channel.onmessage = (e) => {
      if (e.type === "progress") onProgress?.(e.done, e.total);
      else if (e.type === "done") resolve(e.result);
      else reject(e.error);
    };
    invoke<number>(cmd, { ...args, channel })
      .then((job) => onJob?.(job))
      .catch(reject);
  });
}

// ---------------------------------------------------------------- sources

export type SourceKind =
  | "disk"
  | "image"
  | "raid"
  | "fsFile"
  | "archiveEntry"
  | "fileStream"
  | "capture"
  | "http"
  | "s3"
  | "sftp"
  | "process"
  | "gdb";

export interface SourceInfo {
  kind: SourceKind;
  label: string;
  path: string | null;
  writable: boolean;
  pid: number | null;
}

export interface VolumeInfo {
  id: string;
  path: string;
  offset: number | null;
  size: number;
  content: string | null;
  fileSystem: string | null;
  label: string | null;
  mountPoint: string | null;
}

export interface DiskInfo {
  id: string;
  path: string;
  size: number;
  model: string | null;
  sectorSize: number;
  removable: boolean;
  internal: boolean | null;
  virtualDisk: boolean;
  partitionScheme: string | null;
  volumes: VolumeInfo[];
}

export interface Partition {
  number: number;
  offset: number;
  size: number;
  startLba: number;
  typeId: string;
  typeName: string;
  name: string | null;
  guid: string | null;
  bootable: boolean;
  logical: boolean;
}

export interface PartitionTable {
  kind: "mbr" | "gpt" | "apm";
  sectorSize: number;
  partitions: Partition[];
  warnings: string[];
}

export interface FsInfo {
  kind: string;
  label: string | null;
  uuid: string | null;
  blockSize: number;
  totalSize: number;
  freeSize: number | null;
  fields: { name: string; value: string; offset: number | null }[];
}

export interface VolumeView {
  index: number | null;
  offset: number;
  size: number;
  name: string;
  fs: FsInfo | null;
  browsable: boolean;
}

export interface DiskLayout {
  size: number;
  table: PartitionTable | null;
  volumes: VolumeView[];
}

export interface FileEntry {
  id: number;
  aux: number;
  name: string;
  path: string;
  isDir: boolean;
  size: number;
  deleted: boolean;
  metadataOffset: number | null;
  modified: number | null;
  created: number | null;
  attributes: number;
}

export interface Extent {
  logical: number;
  physical: number | null;
  len: number;
}

export interface FsRegion {
  start: number;
  len: number;
  kind: "metadata" | "file" | "directory";
  label: string;
  fileId: number | null;
}

export interface OwnerInfo {
  volume: number | null;
  volumeName: string;
  mapped: boolean;
  region: FsRegion | null;
  detail: string | null;
}

export interface CarvedFile {
  kind: string;
  extension: string;
  offset: number;
  size: number;
  complete: boolean;
}

export type WipePass =
  | { type: "fill"; byte: number }
  | { type: "pattern"; bytes: number[] }
  | { type: "random" };

export type WipeMethod =
  | { method: "zeros" }
  | { method: "random" }
  | { method: "dod3Pass" }
  | { method: "gutmann"; passes: number }
  | { method: "custom"; passes: WipePass[] };

export interface E01Meta {
  caseNumber: string;
  evidenceNumber: string;
  description: string;
  examiner: string;
  notes: string;
}

export type CloneTarget =
  | { kind: "raw"; path: string }
  | { kind: "e01"; path: string; meta: E01Meta }
  | { kind: "doc"; id: number };

export interface CloneReport {
  bytesCopied?: number;
  bytes?: number;
  md5: string;
  verified?: boolean | null;
  firstMismatch?: number | null;
}

export interface WipeReport {
  passes: number;
  bytesWritten: number;
  verified: boolean | null;
  firstMismatch: number | null;
}

export interface ShredReport {
  passes: number;
  bytesPerPass: number;
  finalPath: string;
  deleted: boolean;
}

export interface CommitPreview {
  writes: { offset: number; len: number; old: number[]; new: number[] }[];
  total: number;
  count: number;
}

export interface StreamInfo {
  name: string;
  size: number;
  kind: "extendedAttribute" | "resourceFork" | "alternateDataStream";
}

export interface ArchiveEntry {
  index: number;
  name: string;
  size: number;
  compressedSize: number | null;
  isDir: boolean;
  method: string;
  dataOffset: number | null;
  modified: number | null;
  encrypted: boolean;
}

export type RaidConfig = {
  level: { level: "raid0" } | { level: "raid5"; layout: string };
  stripeSize: number;
  dataOffset: number;
};

export type HttpAuth =
  | { type: "basic"; user: string; password: string }
  | { type: "bearer"; token: string };

export interface HttpOptions {
  auth?: HttpAuth | null;
  headers?: [string, string][];
}

export interface S3Options {
  endpoint: string;
  region: string;
  bucket: string;
  key: string;
  accessKeyId: string;
  secretAccessKey: string;
  sessionToken?: string | null;
  pathStyle?: boolean;
}

export type SftpAuth =
  | { type: "password"; password: string }
  | { type: "privateKey"; key: string; passphrase?: string | null };

export interface SftpOptions {
  host: string;
  port: number;
  user: string;
  auth: SftpAuth;
  path: string;
  writable: boolean;
}

export type SftpOpen =
  | { type: "opened"; info: DocInfo }
  | { type: "hostKey"; host: string; fingerprint: string; changed: boolean };

export type StreamSource =
  | { kind: "udpListen"; bind: string }
  | { kind: "tcpConnect"; addr: string }
  | { kind: "tcpListen"; bind: string };

export interface CaptureView {
  status: {
    state: "idle" | "running" | "stopped" | "full" | "closed" | "failed";
    len: number;
    maxSize: number;
    segments: number;
    localAddr: string | null;
    error: string | null;
  };
  segments: { offset: number; len: number; peer: string }[];
}

// ---------------------------------------------------------------- process memory

export interface ProcessInfo {
  pid: number;
  parentPid: number | null;
  name: string;
  path: string | null;
  user: string | null;
  arch: string | null;
  bits: number | null;
  memory: number;
  virtualMemory: number;
  iconPath: string | null;
  isSelf: boolean;
}

export interface Diagnostic {
  platform: string;
  pid: number;
  operation: string;
  osCode: number | null;
  summary: string;
  steps: string[];
  protected: boolean;
  needsElevation: boolean;
}

export interface Privileges {
  platform: string;
  elevated: boolean;
  ptraceScope: number | null;
  debugPrivilege: boolean | null;
}

export type ProcOpen =
  | { type: "opened"; info: DocInfo }
  | { type: "denied"; diagnostic: Diagnostic };

export interface MemoryRegion {
  start: number;
  size: number;
  protection: { read: boolean; write: boolean; execute: boolean };
  kind: "image" | "mapped" | "private" | "stack" | "heap" | "unknown";
  shared: boolean;
  path: string | null;
  label: string | null;
}

export interface Module {
  name: string;
  path: string;
  base: number;
  size: number;
}

export interface MemMap {
  regions: MemoryRegion[];
  modules: Module[];
  gaps: [number, number][];
  pointerSize: number;
}

export type ValueType = "u8" | "u16" | "u32" | "u64" | "i8" | "i16" | "i32" | "i64" | "f32" | "f64";

export interface RegionFilter {
  writableOnly: boolean;
  includeExecutable: boolean;
  includeMapped: boolean;
  addressRange?: [number, number] | null;
}

export interface ScanOptions {
  valueType: ValueType;
  alignment?: number | null;
  regions?: RegionFilter;
  bigEndian?: boolean;
  floatEpsilon?: number;
}

export type FirstScan =
  | { kind: "exact"; value: string }
  | { kind: "range"; min: string; max: string }
  | { kind: "unknown" };

export type NextScan =
  | { kind: "equal"; value: string }
  | { kind: "notEqual"; value: string }
  | { kind: "between"; min: string; max: string }
  | { kind: "changed" }
  | { kind: "unchanged" }
  | { kind: "increased" }
  | { kind: "decreased" }
  | { kind: "increasedBy"; value: string }
  | { kind: "decreasedBy"; value: string };

export interface ScanSummary {
  count: number;
  scannedBytes: number;
  scans: number;
  memoryBytes: number;
}

export interface ScanHit {
  address: number;
  value: string;
  current: string | null;
}

export interface ScanPage {
  count: number;
  scans: number;
  busy: boolean;
  valueType: ValueType | null;
  hits: ScanHit[];
}

export interface FrozenView {
  id: number;
  address: number;
  label: string | null;
  enabled: boolean;
  writes: number;
  lastError: string | null;
  valueType: ValueType | null;
  value: string | null;
  current: string | null;
}

export interface PointerScanOptions {
  maxDepth: number;
  maxOffset: number;
  maxResults: number;
}

export interface ChainView {
  chain: { module: string; moduleOffset: number; offsets: number[] };
  text: string;
  resolved: number | null;
}

export type DumpWhat =
  | { kind: "range"; start: number; len: number }
  | { kind: "module"; name: string };

export interface DumpReport {
  start: number;
  bytesWritten: number;
  unreadable: { start: number; end: number }[];
}

// ---------------------------------------------------------------- wrappers

type P = (done: number, total: number) => void;
type J = (job: number) => void;

export const src = {
  info: (id: number) => invoke<SourceInfo | null>("src_info", { id }),
  commitPreview: (id: number) => invoke<CommitPreview>("src_commit_preview", { id }),
  commit: (id: number) =>
    invoke<{ bytes: number; journal: string | null; info: DocInfo }>("src_commit", { id }),
  rollback: (id: number, journal: string) => invoke<DocInfo>("src_rollback", { id, journal }),

  diskList: () => invoke<DiskInfo[]>("disk_list"),
  diskOpen: (path: string, writable: boolean, sectorSize: number | null = null) =>
    invoke<DocInfo>("disk_open", { path, writable, sectorSize }),
  imageOpen: (path: string) => invoke<DocInfo>("image_open", { path }),
  raidOpen: (members: (number | null)[], config: RaidConfig) =>
    invoke<DocInfo>("raid_open", { members, config }),
  layout: (id: number) => invoke<DiskLayout>("disk_layout", { id }),
  fsList: (id: number, volume: number | null, dir: FileEntry | null) =>
    invoke<{ dir: FileEntry; entries: FileEntry[] }>("fs_list", { id, volume, dir }),
  fsDeleted: (id: number, volume: number | null) =>
    invoke<FileEntry[]>("fs_deleted", { id, volume }),
  fsExtents: (id: number, volume: number | null, file: FileEntry) =>
    invoke<Extent[]>("fs_extents", { id, volume, file }),
  fsMapBuild: (id: number, volume: number | null, onJob?: J) =>
    runJob<{ files: number; metadata: number }>("fs_map_build", { id, volume }, undefined, onJob),
  fsOwnerAt: (id: number, offset: number) =>
    invoke<OwnerInfo | null>("fs_owner_at", { id, offset }),
  fsOpenFile: (id: number, volume: number | null, file: FileEntry) =>
    invoke<DocInfo>("fs_open_file", { id, volume, file }),
  fsExtract: (id: number, volume: number | null, file: FileEntry, dest: string, p?: P) =>
    runJob<CloneReport>("fs_extract", { id, volume, file, dest }, p),
  carve: (id: number, start: number, end: number | null, alignment: number, p?: P, j?: J) =>
    runJob<CarvedFile[]>("disk_carve", { id, start, end, alignment }, p, j),
  clone: (id: number, target: CloneTarget, verify: boolean, p?: P, j?: J) =>
    runJob<CloneReport>("disk_clone", { id, target, verify }, p, j),
  wipe: (
    id: number,
    method: WipeMethod,
    verify: boolean,
    start: number,
    len: number | null,
    p?: P,
    j?: J,
  ) => runJob<WipeReport>("disk_wipe", { id, method, verify, start, len }, p, j),
  shred: (path: string, method: WipeMethod, rename: boolean, del: boolean, p?: P, j?: J) =>
    runJob<ShredReport>("file_shred", { path, method, rename, delete: del }, p, j),

  streams: (path: string) => invoke<StreamInfo[]>("file_streams", { path }),
  streamOpen: (path: string, name: string) => invoke<DocInfo>("file_stream_open", { path, name }),
  streamSave: (id: number) => invoke<void>("file_stream_save", { id }),
  streamRemove: (path: string, name: string) => invoke<void>("file_stream_remove", { path, name }),

  archiveList: (path: string) =>
    invoke<{ kind: string; entries: ArchiveEntry[] }>("archive_list", { path }),
  archiveOpenEntry: (path: string, name: string) =>
    invoke<DocInfo>("archive_open_entry", { path, name }),
  archiveRepack: (id: number, dest: string | null) =>
    invoke<string>("archive_repack", { id, dest }),

  openHttp: (url: string, options: HttpOptions) =>
    invoke<DocInfo>("net_open_http", { url, options }),
  openS3: (options: S3Options) => invoke<DocInfo>("net_open_s3", { options }),
  openSftp: (options: SftpOptions, fingerprint: string | null) =>
    invoke<SftpOpen>("net_open_sftp", { options, fingerprint }),
  captureStart: (source: StreamSource, maxSize: number) =>
    invoke<DocInfo>("net_capture_start", { options: { source, maxSize } }),
  captureStatus: (id: number) => invoke<CaptureView>("net_capture_status", { id }),
  captureControl: (id: number, action: "stop" | "resume" | "clear") =>
    invoke<void>("net_capture_control", { id, action }),
};

export const mem = {
  processes: () => invoke<ProcessInfo[]>("proc_list"),
  privileges: () => invoke<Privileges>("proc_privileges"),
  open: (pid: number, writable: boolean) => invoke<ProcOpen>("proc_open", { pid, writable }),
  gdbConnect: (address: string) => invoke<DocInfo>("gdb_connect", { address }),
  map: (id: number, refresh = false) => invoke<MemMap>("mem_map", { id, refresh }),
  scanFirst: (id: number, options: ScanOptions, condition: FirstScan, p?: P, j?: J) =>
    runJob<ScanSummary>("scan_first", { id, options, condition }, p, j),
  scanNext: (id: number, condition: NextScan, p?: P, j?: J) =>
    runJob<ScanSummary>("scan_next", { id, condition }, p, j),
  scanPage: (id: number, start: number, limit: number, live: boolean) =>
    invoke<ScanPage>("scan_page", { id, start, limit, live }),
  scanReset: (id: number) => invoke<void>("scan_reset", { id }),
  writeValue: (id: number, address: number, valueType: ValueType, value: string) =>
    invoke<void>("mem_write_value", { id, address, valueType, value }),
  freeze: (id: number, address: number, valueType: ValueType, value: string, label?: string) =>
    invoke<number>("mem_freeze", { id, address, valueType, value, label: label ?? null }),
  frozen: (id: number) => invoke<FrozenView[]>("mem_frozen", { id }),
  freezeControl: (id: number, frozen: number, action: "remove" | "enable" | "disable") =>
    invoke<boolean>("mem_freeze_control", { id, frozen, action }),
  pointerScan: (id: number, target: number, options: PointerScanOptions, p?: P, j?: J) =>
    runJob<{ count: number }>("ptr_scan", { id, target, options }, p, j),
  pointerRescan: (id: number, target: number) => invoke<number>("ptr_rescan", { id, target }),
  pointerResults: (id: number, start: number, limit: number) =>
    invoke<{ total: number; chains: ChainView[] }>("ptr_results", { id, start, limit }),
  dump: (id: number, what: DumpWhat, path: string, p?: P, j?: J) =>
    runJob<DumpReport>("mem_dump", { id, what, path }, p, j),
};

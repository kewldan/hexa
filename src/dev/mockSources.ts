// Browser-mock answers for the source and process-memory commands: small
// fake lists so the dialogs and panels render under `vite dev` and in tests.
// Returns `undefined` for commands it does not know.

const DISKS = [
  {
    id: "disk0",
    path: "/dev/disk0",
    size: 500_107_862_016,
    model: "APPLE SSD AP0512",
    sectorSize: 4096,
    removable: false,
    internal: true,
    virtualDisk: false,
    partitionScheme: "GUID_partition_scheme",
    volumes: [
      {
        id: "disk0s1",
        path: "/dev/disk0s1",
        offset: 20480,
        size: 524_288_000,
        content: "Apple_APFS_ISC",
        fileSystem: null,
        label: null,
        mountPoint: null,
      },
    ],
  },
  {
    id: "disk4",
    path: "/dev/disk4",
    size: 31_914_983_424,
    model: "SanDisk Ultra",
    sectorSize: 512,
    removable: true,
    internal: false,
    virtualDisk: false,
    partitionScheme: "FDisk_partition_scheme",
    volumes: [],
  },
];

const PROCESSES = [
  { pid: 1, name: "launchd", user: "root", bits: 64, memory: 12_000_000 },
  { pid: 412, name: "Finder", user: "user", bits: 64, memory: 180_000_000 },
  { pid: 977, name: "game.exe", user: "user", bits: 32, memory: 640_000_000 },
  { pid: 1204, name: "hexa", user: "user", bits: 64, memory: 95_000_000, isSelf: true },
].map((p) => ({
  parentPid: 1,
  path: `/Applications/${p.name}`,
  arch: p.bits === 32 ? "x86" : "arm64",
  virtualMemory: p.memory * 4,
  iconPath: null,
  isSelf: false,
  ...p,
}));

// biome-ignore lint/suspicious/noExplicitAny: test double
export function mockSources(cmd: string, a: any): unknown {
  switch (cmd) {
    case "src_info":
      return null;
    case "disk_list":
      return DISKS;
    case "proc_list":
      return PROCESSES;
    case "proc_privileges":
      return { platform: "macos", elevated: false, ptraceScope: null, debugPrivilege: null };
    case "proc_open":
      return {
        type: "denied",
        diagnostic: {
          platform: "macos",
          pid: a.pid,
          operation: "task_for_pid",
          osCode: 5,
          summary: "macOS не разрешает читать память этого процесса",
          steps: ["Запустите Hexa через sudo", "Или подпишите цель с get-task-allow"],
          protected: false,
          needsElevation: true,
        },
      };
    case "disk_layout":
      return { size: 0, table: null, volumes: [] };
    case "file_streams":
      return [];
    case "archive_list":
      return { kind: "zip", entries: [] };
    case "mem_map":
    case "scan_page":
    case "mem_frozen":
    case "ptr_results":
    case "fs_owner_at":
      return Promise.reject({ kind: "invalid", message: "mock: no process document" });
    default:
      if (/^(disk|image|raid|fs|net|archive|file|src|mem|scan|ptr|gdb|proc)_/.test(cmd))
        return Promise.reject({
          kind: "unsupported",
          message: `mock: ${cmd} недоступна в браузере`,
        });
      return undefined;
  }
}

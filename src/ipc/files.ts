// Multi-file commands (SRCH-23, FILE-13) and desktop helpers (UI-12, IO-06).
import { Channel, invoke } from "@tauri-apps/api/core";
import type { CmdError, DocInfo, Query } from "./api";

export type FilesEvent =
  | { type: "hits"; path: string; offsets: number[]; len: number }
  | { type: "progress"; files: number; current: string }
  | { type: "skipped"; path: string; reason: string }
  | { type: "done"; files: number; hits: number; truncated: boolean }
  | { type: "error"; error: CmdError };

export const files = {
  search: (
    root: string,
    mask: string,
    recursive: boolean,
    query: Query,
    onEvent: (e: FilesEvent) => void,
  ) => {
    const channel = new Channel<FilesEvent>();
    channel.onmessage = onEvent;
    return invoke<number>("files_search", {
      root,
      mask,
      recursive,
      query,
      maxPerFile: 1000,
      maxFiles: 100_000,
      channel,
    });
  },
  split: (id: number, partSize: number, base: string) =>
    invoke<string[]>("file_split", { id, partSize, base }),
  concat: (paths: string[]) => invoke<DocInfo>("files_concat", { paths }),
  appInfo: () => invoke<{ version: string; portable: boolean; configDir: string }>("app_info"),
  print: () => invoke<void>("app_print"),
};

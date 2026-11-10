// Shared bits of the source dialogs and the disk / memory panels.
import { useEffect, useRef, useState } from "react";
import { api, isCmdError } from "../ipc/api";
import { type SourceInfo, src } from "../ipc/sources";
import "../styles/sources.css";

const infos = new Map<number, SourceInfo | null>();
const waiting = new Map<number, Promise<SourceInfo | null>>();

/** Where document `id` was opened from (cached; `null` = a plain file). */
export function sourceInfo(id: number): Promise<SourceInfo | null> {
  const known = infos.get(id);
  if (known !== undefined) return Promise.resolve(known);
  let p = waiting.get(id);
  if (!p) {
    p = src
      .info(id)
      .catch(() => null)
      .then((info) => {
        infos.set(id, info ?? null);
        waiting.delete(id);
        return info ?? null;
      });
    waiting.set(id, p);
  }
  return p;
}

export function useSourceInfo(id: number | undefined): SourceInfo | null | undefined {
  const [info, setInfo] = useState<SourceInfo | null | undefined>(
    id === undefined ? undefined : infos.get(id),
  );
  useEffect(() => {
    if (id === undefined) return setInfo(undefined);
    let alive = true;
    setInfo(infos.get(id));
    void sourceInfo(id).then((i) => alive && setInfo(i));
    return () => {
      alive = false;
    };
  }, [id]);
  return info;
}

export function isMemorySource(info: SourceInfo | null | undefined): boolean {
  return info?.kind === "process" || info?.kind === "gdb";
}

/** 0x-prefixed hex address, at least 8 digits. */
export function addr(n: number): string {
  return `0x${n.toString(16).toUpperCase().padStart(8, "0")}`;
}

/** Parses `0x…`, plain hex with letters, or decimal. */
export function parseAddr(text: string): number | null {
  const t = text.trim().replace(/_/g, "");
  if (!t) return null;
  const n = /^0x/i.test(t)
    ? Number.parseInt(t.slice(2), 16)
    : /[a-f]/i.test(t)
      ? Number.parseInt(t, 16)
      : Number(t);
  return Number.isFinite(n) && n >= 0 ? n : null;
}

export interface JobState {
  running: boolean;
  done: number;
  total: number;
  job: number | null;
}

/** Progress and cancellation of one background job at a time. */
export function useJob() {
  const [st, setSt] = useState<JobState>({ running: false, done: 0, total: 0, job: null });
  const alive = useRef(true);
  useEffect(
    () => () => {
      alive.current = false;
    },
    [],
  );
  const set = (p: Partial<JobState>) => alive.current && setSt((s) => ({ ...s, ...p }));
  async function run<T>(
    start: (
      onProgress: (done: number, total: number) => void,
      onJob: (job: number) => void,
    ) => Promise<T>,
  ): Promise<T | null> {
    set({ running: true, done: 0, total: 0, job: null });
    try {
      return await start(
        (done, total) => set({ done, total }),
        (job) => set({ job }),
      );
    } catch (e) {
      if (isCmdError(e) && e.kind === "cancelled") return null;
      throw e;
    } finally {
      set({ running: false, job: null });
    }
  }
  const cancel = () => {
    if (st.job !== null) void api.cancelJob(st.job);
  };
  return { ...st, run, cancel };
}

export function JobBar({ job, label }: { job: ReturnType<typeof useJob>; label: string }) {
  if (!job.running) return null;
  const pct = job.total > 0 ? Math.round((job.done / job.total) * 100) : null;
  return (
    <div className="src-job" role="status">
      <span>
        {label}
        {pct !== null && ` · ${pct}%`}
      </span>
      <span className="progress src-progress">
        <span style={{ width: `${pct ?? 0}%` }} />
      </span>
      <button type="button" className="chip" onClick={job.cancel} disabled={job.job === null}>
        Отмена
      </button>
    </div>
  );
}

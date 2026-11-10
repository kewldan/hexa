// Sources beyond plain files: disks, processes, remote objects, archives,
// disk images, network capture, imaging, wiping, shredding and alternate
// streams (DISK-01…12, FILE-14…17, CORE-02, PROC-01/09/10).
import { open as openFile, save as saveFile } from "@tauri-apps/plugin-dialog";
import { useEffect, useMemo, useState } from "react";
import { api, type DocInfo } from "../../ipc/api";
import {
  type ArchiveEntry,
  type CaptureView,
  type CommitPreview,
  type Diagnostic,
  type DiskInfo,
  type HttpAuth,
  mem,
  type Privileges,
  type ProcessInfo,
  type RaidConfig,
  type SftpOptions,
  type StreamInfo,
  type StreamSource,
  src,
  type WipeMethod,
} from "../../ipc/sources";
import { fmtSize, groupDec, hexN } from "../../lib/format";
import * as A from "../../state/actions";
import { selection, useActiveTab, useTabs } from "../../state/tabs";
import { closeDialog, type Dialog, toast } from "../../state/ui";
import { addr, JobBar, sourceInfo, useJob, useSourceInfo } from "../sourceKit";
import { Modal } from "./Dialogs";

export type SourceDialogName =
  | "disk"
  | "process"
  | "remote"
  | "archive"
  | "image"
  | "capture"
  | "clone"
  | "wipe"
  | "shred"
  | "streams"
  | "commit"
  | "raid";

export type SourceDialog = { kind: "src"; name: SourceDialogName; path?: string };

export function isSourceDialog(d: Dialog): d is SourceDialog {
  return d.kind === "src";
}

export function SourceDialogs({ d }: { d: SourceDialog }) {
  switch (d.name) {
    case "disk":
      return <OpenDiskDialog />;
    case "process":
      return <OpenProcessDialog />;
    case "remote":
      return <OpenRemoteDialog />;
    case "archive":
      return <ArchiveDialog path={d.path} />;
    case "image":
      return <OpenImageDialog />;
    case "capture":
      return <CaptureDialog />;
    case "clone":
      return <CloneDialog />;
    case "wipe":
      return <WipeDialog />;
    case "shred":
      return <ShredDialog path={d.path} />;
    case "streams":
      return <StreamsDialog path={d.path} />;
    case "commit":
      return <CommitDialog />;
    case "raid":
      return <RaidDialog />;
    default:
      return null;
  }
}

async function adoptAndClose(p: Promise<DocInfo>, note?: string) {
  const info = await p;
  closeDialog();
  await A.adopt(info);
  if (note) toast(note);
  return info;
}

function CancelBtn({ label = "Отмена" }: { label?: string }) {
  return (
    <button type="button" className="btn" onClick={closeDialog}>
      {label}
    </button>
  );
}

const hx = (b: number[]) => b.map((x) => hexN(x, 2)).join(" ");

// ---------------------------------------------------------------- disks (DISK-01…03)

interface DiskPick {
  path: string;
  label: string;
}

function OpenDiskDialog() {
  const [disks, setDisks] = useState<DiskInfo[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [pick, setPick] = useState<DiskPick | null>(null);
  const [writeStep, setWriteStep] = useState(false);
  const [understood, setUnderstood] = useState(false);
  const [busy, setBusy] = useState(false);
  const load = () => {
    setDisks(null);
    setError(null);
    src
      .diskList()
      .then(setDisks)
      .catch((e) => setError(e?.message ?? String(e)));
  };
  useEffect(load, []);
  const open = async (writable: boolean) => {
    if (!pick) return;
    setBusy(true);
    try {
      await adoptAndClose(
        src.diskOpen(pick.path, writable),
        writable
          ? `${pick.label} открыт для записи — правки уходят на диск только по «Записать в источник»`
          : `${pick.label} открыт только для чтения`,
      );
    } catch (e) {
      A.report(e);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Modal
      title="Открыть диск"
      wide
      footer={
        <>
          <button type="button" className="chip" onClick={load}>
            Обновить
          </button>
          <div className="grow" />
          <CancelBtn />
          {writeStep ? (
            <button
              type="button"
              className="btn danger"
              disabled={!pick || !understood || busy}
              onClick={() => open(true)}
            >
              Открыть для записи
            </button>
          ) : (
            <>
              <button
                type="button"
                className="btn"
                disabled={!pick || busy}
                onClick={() => setWriteStep(true)}
              >
                Открыть для записи…
              </button>
              <button
                type="button"
                className="btn pri"
                disabled={!pick || busy}
                onClick={() => open(false)}
              >
                Открыть только для чтения
              </button>
            </>
          )}
        </>
      }
    >
      {writeStep && pick ? (
        <>
          <div className="src-warn">
            Запись на «{pick.label}» может безвозвратно повредить данные и файловую систему.
            Смонтированные тома лучше сначала отключить. Перед записью Hexa покажет список изменений
            и сохранит журнал для отката.
          </div>
          <label className="chk">
            <input
              type="checkbox"
              checked={understood}
              onChange={(e) => setUnderstood(e.target.checked)}
            />
            Понимаю риск, открыть {pick.path} для записи
          </label>
          <button type="button" className="chip" onClick={() => setWriteStep(false)}>
            ← Назад к списку
          </button>
        </>
      ) : (
        <>
          {error && (
            <div className="src-warn">
              {error}
              <br />
              Для доступа к дискам обычно нужны права администратора.
            </div>
          )}
          {!disks && !error && <div className="note">Ищу диски…</div>}
          {disks && disks.length === 0 && <div className="note">Дисков не найдено.</div>}
          <div className="list src-list">
            {disks?.map((d) => (
              <div key={d.id}>
                <button
                  type="button"
                  className={`src-row${pick?.path === d.path ? " cur" : ""}`}
                  onClick={() => setPick({ path: d.path, label: d.model ?? d.id })}
                  onDoubleClick={() => setPick({ path: d.path, label: d.model ?? d.id })}
                >
                  <span className="t">
                    <b>
                      {d.model ?? d.id}
                      {d.removable && <span className="tag"> · съёмный</span>}
                      {d.virtualDisk && <span className="tag"> · виртуальный</span>}
                    </b>
                    <span>
                      {d.path} · сектор {d.sectorSize} Б
                      {d.partitionScheme ? ` · ${d.partitionScheme}` : ""}
                    </span>
                  </span>
                  <span className="v">{fmtSize(d.size)}</span>
                </button>
                {d.volumes.map((v) => (
                  <button
                    type="button"
                    key={v.id}
                    className={`src-row${pick?.path === v.path ? " cur" : ""}`}
                    style={{ paddingLeft: 26 }}
                    onClick={() => setPick({ path: v.path, label: v.label ?? v.id })}
                  >
                    <span className="t">
                      <b>{v.label || v.id}</b>
                      <span>
                        {v.path}
                        {v.fileSystem ? ` · ${v.fileSystem}` : ""}
                        {v.mountPoint ? ` · ${v.mountPoint}` : ""}
                      </span>
                    </span>
                    <span className="v">{fmtSize(v.size)}</span>
                  </button>
                ))}
              </div>
            ))}
          </div>
          <div className="note">
            Диск открывается только для чтения. Таблица разделов и файлы — в панели «Диск».
          </div>
        </>
      )}
    </Modal>
  );
}

function OpenImageDialog() {
  const [busy, setBusy] = useState(false);
  const pick = async () => {
    const p = await openFile({
      multiple: false,
      filters: [
        {
          name: "Образы дисков",
          extensions: [
            "img",
            "dd",
            "raw",
            "001",
            "iso",
            "vhd",
            "vhdx",
            "vmdk",
            "qcow2",
            "qcow",
            "dmg",
            "e01",
          ],
        },
        { name: "Все файлы", extensions: ["*"] },
      ],
    });
    if (typeof p !== "string") return;
    setBusy(true);
    try {
      await adoptAndClose(src.imageOpen(p), "Образ открыт только для чтения");
    } catch (e) {
      A.report(e);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Modal
      title="Открыть образ диска"
      footer={
        <>
          <CancelBtn />
          <button type="button" className="btn pri" onClick={pick} disabled={busy}>
            Выбрать файл…
          </button>
        </>
      }
    >
      <div className="note">
        Поддерживаются VHD, VHDX, VMDK (sparse, stream-optimized, дескрипторы), QCOW2 (со сжатием и
        backing-файлами), DMG, E01 и raw / split raw (<span className="mono">name.001</span>
        открывает весь набор). Образ показывается как виртуальный диск: разделы и файлы — в панели
        «Диск».
      </div>
    </Modal>
  );
}

// ---------------------------------------------------------------- processes (PROC-01, 09, 10)

function DiagnosticView({ d }: { d: Diagnostic }) {
  return (
    <div className="src-warn" style={{ color: "var(--ink)" }}>
      <b>{d.summary}</b>
      {d.steps.length > 0 && (
        <ol className="src-steps">
          {d.steps.map((s) => (
            <li key={s}>{s}</li>
          ))}
        </ol>
      )}
      {d.needsElevation && (
        <div className="note">
          Нужны права администратора: перезапустите Hexa с повышением прав.
        </div>
      )}
      {d.protected && <div className="note">Процесс защищён системой — доступ к нему закрыт.</div>}
    </div>
  );
}

function OpenProcessDialog() {
  const [list, setList] = useState<ProcessInfo[] | null>(null);
  const [priv, setPriv] = useState<Privileges | null>(null);
  const [q, setQ] = useState("");
  const [user, setUser] = useState("");
  const [bits, setBits] = useState("");
  const [pick, setPick] = useState<ProcessInfo | null>(null);
  const [denied, setDenied] = useState<Diagnostic | null>(null);
  const [gdb, setGdb] = useState("127.0.0.1:1234");
  const [busy, setBusy] = useState(false);
  const load = () => {
    setList(null);
    mem
      .processes()
      .then(setList)
      .catch((e) => {
        setList([]);
        A.report(e);
      });
  };
  useEffect(() => {
    load();
    mem
      .privileges()
      .then(setPriv)
      .catch(() => {});
  }, []);
  const users = useMemo(
    () => [...new Set((list ?? []).map((p) => p.user).filter((u): u is string => !!u))].sort(),
    [list],
  );
  const shown = useMemo(() => {
    const f = q.trim().toLowerCase();
    return (list ?? [])
      .filter(
        (p) =>
          (!f ||
            p.name.toLowerCase().includes(f) ||
            String(p.pid).includes(f) ||
            (p.path ?? "").toLowerCase().includes(f)) &&
          (!user || p.user === user) &&
          (!bits || String(p.bits) === bits),
      )
      .sort((a, b) => a.name.localeCompare(b.name) || a.pid - b.pid);
  }, [list, q, user, bits]);
  const open = async (p: ProcessInfo | null, writable: boolean) => {
    if (!p) return;
    setBusy(true);
    setDenied(null);
    try {
      const r = await mem.open(p.pid, writable);
      if (r.type === "denied") {
        setDenied(r.diagnostic);
        return;
      }
      closeDialog();
      await A.adopt(r.info);
      toast(
        `${p.name} (PID ${p.pid}) открыт ${writable ? "с записью" : "только для чтения"} — регионы и сканер в панели «Память»`,
      );
    } catch (e) {
      A.report(e);
    } finally {
      setBusy(false);
    }
  };
  const connectGdb = async () => {
    setBusy(true);
    try {
      await adoptAndClose(mem.gdbConnect(gdb.trim()), `Подключено к GDB ${gdb.trim()}`);
    } catch (e) {
      A.report(e);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Modal
      title="Открыть процесс"
      wide
      footer={
        <>
          <button type="button" className="chip" onClick={load}>
            Обновить
          </button>
          <div className="grow" />
          <CancelBtn />
          <button
            type="button"
            className="btn"
            disabled={!pick || busy}
            onClick={() => open(pick, true)}
          >
            С записью
          </button>
          <button
            type="button"
            className="btn pri"
            disabled={!pick || busy}
            onClick={() => open(pick, false)}
          >
            Открыть
          </button>
        </>
      }
    >
      {priv && !priv.elevated && (
        <div className="note">
          Hexa запущена без прав администратора: чужие и системные процессы могут быть недоступны.
          {priv.ptraceScope != null &&
            priv.ptraceScope > 0 &&
            ` ptrace_scope = ${priv.ptraceScope}.`}
        </div>
      )}
      <div className="src-inline">
        <input
          className="inp"
          placeholder="Имя, PID или путь"
          aria-label="Поиск процесса"
          value={q}
          onChange={(e) => setQ(e.target.value)}
        />
        <select
          className="sel-input"
          aria-label="Пользователь"
          value={user}
          onChange={(e) => setUser(e.target.value)}
        >
          <option value="">все пользователи</option>
          {users.map((u) => (
            <option key={u} value={u}>
              {u}
            </option>
          ))}
        </select>
        <select
          className="sel-input"
          aria-label="Разрядность"
          value={bits}
          onChange={(e) => setBits(e.target.value)}
        >
          <option value="">32 и 64 бит</option>
          <option value="64">64 бит</option>
          <option value="32">32 бит</option>
        </select>
      </div>
      {denied && <DiagnosticView d={denied} />}
      {!list && <div className="note">Загружаю список процессов…</div>}
      <div className="list src-list" role="list" aria-label="Процессы">
        {shown.slice(0, 1000).map((p) => (
          <button
            type="button"
            key={p.pid}
            className={`src-row${pick?.pid === p.pid ? " cur" : ""}`}
            onClick={() => setPick(p)}
            onDoubleClick={() => open(p, false)}
            title={p.path ?? undefined}
          >
            <span className="t">
              <b>
                {p.name}
                {p.isSelf && <span className="tag"> · Hexa</span>}
              </b>
              <span>
                PID {p.pid}
                {p.user ? ` · ${p.user}` : ""}
                {p.bits ? ` · ${p.bits} бит` : ""}
                {p.arch ? ` · ${p.arch}` : ""}
              </span>
            </span>
            <span className="v">{fmtSize(p.memory)}</span>
          </button>
        ))}
      </div>
      {list && shown.length === 0 && <div className="note">Ничего не найдено.</div>}
      <div className="row-f">
        <span>GDB remote</span>
        <div className="src-inline">
          <input
            className="inp"
            aria-label="Адрес GDB-сервера"
            value={gdb}
            onChange={(e) => setGdb(e.target.value)}
          />
          <button type="button" className="btn" onClick={connectGdb} disabled={busy || !gdb.trim()}>
            Подключиться
          </button>
        </div>
      </div>
    </Modal>
  );
}

// ---------------------------------------------------------------- remote (CORE-02, FILE-16)

type RemoteKind = "http" | "s3" | "sftp";

function Field({
  label,
  value,
  onChange,
  type = "text",
  placeholder,
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  type?: string;
  placeholder?: string;
}) {
  return (
    <div className="row-f">
      <span>{label}</span>
      <input
        className="inp"
        type={type}
        aria-label={label}
        value={value}
        placeholder={placeholder}
        onChange={(e) => onChange(e.target.value)}
      />
    </div>
  );
}

function OpenRemoteDialog() {
  const [kind, setKind] = useState<RemoteKind>("http");
  const [busy, setBusy] = useState(false);
  // HTTP
  const [url, setUrl] = useState("https://");
  const [auth, setAuth] = useState<"none" | "basic" | "bearer">("none");
  const [user, setUser] = useState("");
  const [password, setPassword] = useState("");
  const [token, setToken] = useState("");
  const [headers, setHeaders] = useState("");
  // S3
  const [endpoint, setEndpoint] = useState("https://s3.amazonaws.com");
  const [region, setRegion] = useState("us-east-1");
  const [bucket, setBucket] = useState("");
  const [key, setKey] = useState("");
  const [accessKey, setAccessKey] = useState("");
  const [secret, setSecret] = useState("");
  const [session, setSession] = useState("");
  const [pathStyle, setPathStyle] = useState(false);
  // SFTP
  const [host, setHost] = useState("");
  const [port, setPort] = useState("22");
  const [sUser, setSUser] = useState("");
  const [sAuth, setSAuth] = useState<"password" | "key">("password");
  const [sPass, setSPass] = useState("");
  const [keyPath, setKeyPath] = useState("");
  const [passphrase, setPassphrase] = useState("");
  const [rPath, setRPath] = useState("");
  const [writable, setWritable] = useState(false);
  const [hostKey, setHostKey] = useState<{
    host: string;
    fingerprint: string;
    changed: boolean;
  } | null>(null);

  const run = async (f: () => Promise<void>) => {
    setBusy(true);
    try {
      await f();
    } catch (e) {
      A.report(e);
    } finally {
      setBusy(false);
    }
  };

  const openHttp = () =>
    run(async () => {
      const a: HttpAuth | null =
        auth === "basic"
          ? { type: "basic", user, password }
          : auth === "bearer"
            ? { type: "bearer", token }
            : null;
      const hs = headers
        .split("\n")
        .map((l) => l.trim())
        .filter(Boolean)
        .map((l): [string, string] => {
          const i = l.indexOf(":");
          return i < 0 ? [l, ""] : [l.slice(0, i).trim(), l.slice(i + 1).trim()];
        });
      await adoptAndClose(src.openHttp(url.trim(), { auth: a, headers: hs }));
    });

  const openS3 = () =>
    run(async () => {
      await adoptAndClose(
        src.openS3({
          endpoint: endpoint.trim(),
          region: region.trim(),
          bucket: bucket.trim(),
          key: key.trim(),
          accessKeyId: accessKey.trim(),
          secretAccessKey: secret,
          sessionToken: session.trim() || null,
          pathStyle,
        }),
      );
    });

  const openSftp = (fingerprint: string | null) =>
    run(async () => {
      const keyText = sAuth === "key" ? await api.readTextFile(keyPath) : "";
      const options: SftpOptions = {
        host: host.trim(),
        port: Number(port) || 22,
        user: sUser.trim(),
        auth:
          sAuth === "password"
            ? { type: "password", password: sPass }
            : { type: "privateKey", key: keyText, passphrase: passphrase || null },
        path: rPath.trim(),
        writable,
      };
      const r = await src.openSftp(options, fingerprint);
      if (r.type === "hostKey") {
        setHostKey(r);
        return;
      }
      setHostKey(null);
      closeDialog();
      await A.adopt(r.info);
      if (writable) toast("Правки уходят на сервер по «Записать в источник»");
    });

  const pickKey = async () => {
    const p = await openFile({ multiple: false });
    if (typeof p === "string") setKeyPath(p);
  };

  const ok =
    kind === "http"
      ? /^https?:\/\/.+/i.test(url.trim())
      : kind === "s3"
        ? !!(endpoint && bucket && key && accessKey && secret)
        : !!(host && sUser && rPath && (sAuth === "password" ? sPass : keyPath));

  if (hostKey)
    return (
      <Modal
        title={hostKey.changed ? "Ключ сервера изменился" : "Неизвестный сервер"}
        footer={
          <>
            <button type="button" className="btn" onClick={() => setHostKey(null)}>
              Отмена
            </button>
            <button
              type="button"
              className={`btn ${hostKey.changed ? "danger" : "pri"}`}
              disabled={busy}
              onClick={() => openSftp(hostKey.fingerprint)}
            >
              Доверять и подключиться
            </button>
          </>
        }
      >
        {hostKey.changed ? (
          <div className="src-warn">
            Ключ {hostKey.host} не совпадает с запомненным. Это может быть переустановка сервера —
            или перехват соединения. Подключайтесь, только если уверены.
          </div>
        ) : (
          <div className="note">
            Hexa ещё не подключалась к {hostKey.host}. Сверьте отпечаток ключа с администратором
            сервера (ssh-keygen -lf), после подтверждения он будет запомнен.
          </div>
        )}
        <div className="src-fp">{hostKey.fingerprint}</div>
      </Modal>
    );

  return (
    <Modal
      title="Открыть по адресу"
      wide
      footer={
        <>
          <CancelBtn />
          <button
            type="button"
            className="btn pri"
            disabled={!ok || busy}
            onClick={() =>
              kind === "http" ? openHttp() : kind === "s3" ? openS3() : openSftp(null)
            }
          >
            {busy ? "Подключаюсь…" : "Открыть"}
          </button>
        </>
      }
    >
      <div className="src-tabs" role="tablist">
        {(
          [
            ["http", "HTTP(S)"],
            ["s3", "S3"],
            ["sftp", "SFTP"],
          ] as const
        ).map(([k, t]) => (
          <button
            type="button"
            role="tab"
            aria-selected={kind === k}
            key={k}
            className={`chip${kind === k ? " on" : ""}`}
            onClick={() => setKind(k)}
          >
            {t}
          </button>
        ))}
      </div>
      {kind === "http" && (
        <>
          <Field label="URL" value={url} onChange={setUrl} />
          <div className="row-f">
            <span>Авторизация</span>
            <select
              className="inp"
              value={auth}
              onChange={(e) => setAuth(e.target.value as typeof auth)}
            >
              <option value="none">нет</option>
              <option value="basic">Basic (логин и пароль)</option>
              <option value="bearer">Bearer-токен</option>
            </select>
          </div>
          {auth === "basic" && (
            <>
              <Field label="Логин" value={user} onChange={setUser} />
              <Field label="Пароль" type="password" value={password} onChange={setPassword} />
            </>
          )}
          {auth === "bearer" && (
            <Field label="Токен" type="password" value={token} onChange={setToken} />
          )}
          <div className="row-f">
            <span>Заголовки</span>
            <textarea
              className="inp"
              style={{ minHeight: 60 }}
              aria-label="Заголовки"
              placeholder="Имя: значение — по одному в строке"
              value={headers}
              onChange={(e) => setHeaders(e.target.value)}
            />
          </div>
          <div className="note">
            Читается по частям через Range-запросы; сервер без Range отдаёт файл целиком (до
            лимита).
          </div>
        </>
      )}
      {kind === "s3" && (
        <>
          <Field label="Endpoint" value={endpoint} onChange={setEndpoint} />
          <Field label="Регион" value={region} onChange={setRegion} />
          <Field label="Bucket" value={bucket} onChange={setBucket} />
          <Field label="Ключ объекта" value={key} onChange={setKey} placeholder="dir/file.bin" />
          <Field label="Access key ID" value={accessKey} onChange={setAccessKey} />
          <Field label="Secret key" type="password" value={secret} onChange={setSecret} />
          <Field label="Session token" type="password" value={session} onChange={setSession} />
          <label className="chk">
            <input
              type="checkbox"
              checked={pathStyle}
              onChange={(e) => setPathStyle(e.target.checked)}
            />
            Path-style адреса (MinIO и другие self-hosted хранилища)
          </label>
        </>
      )}
      {kind === "sftp" && (
        <>
          <div className="row-f">
            <span>Сервер · порт</span>
            <div className="src-inline">
              <input
                className="inp"
                aria-label="Сервер"
                value={host}
                onChange={(e) => setHost(e.target.value)}
              />
              <input
                className="inp"
                aria-label="Порт"
                style={{ flex: "0 0 70px" }}
                value={port}
                onChange={(e) => setPort(e.target.value)}
              />
            </div>
          </div>
          <Field label="Пользователь" value={sUser} onChange={setSUser} />
          <div className="row-f">
            <span>Вход</span>
            <select
              className="inp"
              value={sAuth}
              onChange={(e) => setSAuth(e.target.value as typeof sAuth)}
            >
              <option value="password">пароль</option>
              <option value="key">приватный ключ</option>
            </select>
          </div>
          {sAuth === "password" ? (
            <Field label="Пароль" type="password" value={sPass} onChange={setSPass} />
          ) : (
            <>
              <div className="row-f">
                <span>Файл ключа</span>
                <div className="src-inline">
                  <input className="inp" aria-label="Файл ключа" value={keyPath} readOnly />
                  <button type="button" className="btn" onClick={pickKey}>
                    Выбрать…
                  </button>
                </div>
              </div>
              <Field
                label="Пароль ключа"
                type="password"
                value={passphrase}
                onChange={setPassphrase}
              />
            </>
          )}
          <Field label="Путь к файлу" value={rPath} onChange={setRPath} placeholder="/var/log/x" />
          <label className="chk">
            <input
              type="checkbox"
              checked={writable}
              onChange={(e) => setWritable(e.target.checked)}
            />
            Открыть с записью
          </label>
        </>
      )}
    </Modal>
  );
}

// ---------------------------------------------------------------- archives (FILE-15)

function ArchiveDialog({ path: initial }: { path?: string }) {
  const tab = useActiveTab();
  const info = useSourceInfo(tab?.id);
  const fromArchive = info?.kind === "archiveEntry";
  const [path, setPath] = useState(initial ?? (fromArchive ? (info?.path ?? "") : ""));
  const [list, setList] = useState<{ kind: string; entries: ArchiveEntry[] } | null>(null);
  const [q, setQ] = useState("");
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    if (!initial && fromArchive && info?.path) setPath(info.path);
  }, [fromArchive, info?.path]);
  useEffect(() => {
    if (!path) return;
    setList(null);
    src
      .archiveList(path)
      .then(setList)
      .catch((e) => A.report(e));
  }, [path]);
  const pick = async () => {
    const p = await openFile({
      multiple: false,
      filters: [{ name: "Архивы", extensions: ["zip", "tar", "gz", "tgz", "7z", "jar", "apk"] }],
    });
    if (typeof p === "string") setPath(p);
  };
  useEffect(() => {
    if (!initial && !fromArchive && info !== undefined) void pick();
  }, [info === undefined]);
  const openEntry = async (name: string) => {
    setBusy(true);
    try {
      await adoptAndClose(
        src.archiveOpenEntry(path, name),
        "Запись открыта — «Сохранить в архив» вернёт правки в архив",
      );
    } catch (e) {
      A.report(e);
    } finally {
      setBusy(false);
    }
  };
  const repack = async (copy: boolean) => {
    if (!tab) return;
    let dest: string | null = null;
    if (copy) {
      const p = await saveFile({ defaultPath: path.split(/[\\/]/).pop() });
      if (!p) return;
      dest = p;
    }
    setBusy(true);
    try {
      const out = await src.archiveRepack(tab.id, dest);
      closeDialog();
      toast(`Архив пересобран: ${out}`);
    } catch (e) {
      A.report(e);
    } finally {
      setBusy(false);
    }
  };
  const shown = (list?.entries ?? []).filter(
    (e) => !e.isDir && (!q || e.name.toLowerCase().includes(q.toLowerCase())),
  );
  return (
    <Modal
      title="Архив"
      wide
      footer={
        <>
          {fromArchive && (
            <>
              <button type="button" className="btn" disabled={busy} onClick={() => repack(true)}>
                Сохранить копию архива…
              </button>
              <button
                type="button"
                className="btn pri"
                disabled={busy}
                onClick={() => repack(false)}
              >
                Сохранить в архив
              </button>
            </>
          )}
          <CancelBtn label="Закрыть" />
        </>
      }
    >
      <div className="row-f">
        <span>Файл архива</span>
        <div className="src-inline">
          <input className="inp" aria-label="Файл архива" value={path} readOnly />
          <button type="button" className="btn" onClick={pick}>
            Выбрать…
          </button>
        </div>
      </div>
      {fromArchive && (
        <div className="note">
          Активная вкладка — запись «{tab?.info.name}». «Сохранить в архив» заменит её в архиве
          атомарно; остальные записи копируются без перепаковки.
        </div>
      )}
      {list && (
        <>
          <input
            className="inp"
            placeholder={`Поиск среди ${list.entries.length} записей · ${list.kind}`}
            aria-label="Поиск записи"
            value={q}
            onChange={(e) => setQ(e.target.value)}
          />
          <div className="list src-list">
            {shown.slice(0, 2000).map((e) => (
              <button
                type="button"
                key={e.index}
                className="src-row"
                disabled={busy}
                onClick={() => openEntry(e.name)}
              >
                <span className="t">
                  <b>{e.name}</b>
                  <span>
                    {e.method}
                    {e.compressedSize != null ? ` · сжато ${fmtSize(e.compressedSize)}` : ""}
                    {e.encrypted ? " · зашифровано" : ""}
                  </span>
                </span>
                <span className="v">{fmtSize(e.size)}</span>
              </button>
            ))}
          </div>
        </>
      )}
      {!path && <div className="note">Выберите zip, tar, tar.gz, gz или 7z.</div>}
    </Modal>
  );
}

// ---------------------------------------------------------------- network capture (CORE-02)

const CAPTURE_STATE: Record<CaptureView["status"]["state"], string> = {
  idle: "не запущен",
  running: "идёт захват",
  stopped: "остановлен",
  full: "буфер заполнен",
  closed: "соединение закрыто",
  failed: "ошибка",
};

function CaptureDialog() {
  const tab = useActiveTab();
  const info = useSourceInfo(tab?.id);
  const [mode, setMode] = useState<StreamSource["kind"]>("udpListen");
  const [address, setAddress] = useState("0.0.0.0:9000");
  const [maxMb, setMaxMb] = useState("64");
  const [busy, setBusy] = useState(false);
  const [view, setView] = useState<CaptureView | null>(null);
  const live = info?.kind === "capture" && tab ? tab.id : null;
  useEffect(() => {
    if (live === null) return;
    let alive = true;
    const poll = () =>
      src
        .captureStatus(live)
        .then((v) => alive && setView(v))
        .catch(() => {});
    void poll();
    const t = setInterval(poll, 700);
    return () => {
      alive = false;
      clearInterval(t);
    };
  }, [live]);
  const start = async () => {
    const source: StreamSource =
      mode === "tcpConnect"
        ? { kind: mode, addr: address.trim() }
        : { kind: mode, bind: address.trim() };
    setBusy(true);
    try {
      await adoptAndClose(
        src.captureStart(source, Math.max(1, Number(maxMb) || 64) * 1024 * 1024),
        "Захват запущен — данные дописываются в конец документа",
      );
    } catch (e) {
      A.report(e);
    } finally {
      setBusy(false);
    }
  };
  const control = (action: "stop" | "resume" | "clear") =>
    live !== null && src.captureControl(live, action).catch(A.report);
  if (live !== null)
    return (
      <Modal
        title="Захват сети"
        footer={
          <>
            <button type="button" className="btn" onClick={() => control("clear")}>
              Очистить
            </button>
            {view?.status.state === "running" ? (
              <button type="button" className="btn" onClick={() => control("stop")}>
                Остановить
              </button>
            ) : (
              <button type="button" className="btn" onClick={() => control("resume")}>
                Продолжить
              </button>
            )}
            <CancelBtn label="Закрыть" />
          </>
        }
      >
        {view && (
          <div className="kv">
            <span>Состояние</span>
            <b>{CAPTURE_STATE[view.status.state]}</b>
            <span>Получено</span>
            <b>
              {fmtSize(view.status.len)} из {fmtSize(view.status.maxSize)}
            </b>
            <span>Сегментов</span>
            <b>{groupDec(view.status.segments)}</b>
            {view.status.localAddr && (
              <>
                <span>Локальный адрес</span>
                <b className="mono">{view.status.localAddr}</b>
              </>
            )}
            {view.status.error && (
              <>
                <span>Ошибка</span>
                <b className="bad">{view.status.error}</b>
              </>
            )}
          </div>
        )}
        <div className="list src-list">
          {view?.segments
            .slice(-200)
            .reverse()
            .map((s) => (
              <button
                type="button"
                key={s.offset}
                className="src-row"
                onClick={() => A.selectRange(s.offset, s.offset + Math.max(1, s.len))}
              >
                <span className="t">
                  <b className="mono">{s.peer}</b>
                  <span>{addr(s.offset)}</span>
                </span>
                <span className="v">{fmtSize(s.len)}</span>
              </button>
            ))}
        </div>
      </Modal>
    );
  return (
    <Modal
      title="Захват сети"
      footer={
        <>
          <CancelBtn />
          <button
            type="button"
            className="btn pri"
            disabled={busy || !address.trim()}
            onClick={start}
          >
            Начать захват
          </button>
        </>
      }
    >
      <div className="row-f">
        <span>Источник</span>
        <select
          className="inp"
          value={mode}
          onChange={(e) => {
            const m = e.target.value as StreamSource["kind"];
            setMode(m);
            setAddress(m === "tcpConnect" ? "127.0.0.1:9000" : "0.0.0.0:9000");
          }}
        >
          <option value="udpListen">UDP — слушать порт</option>
          <option value="tcpListen">TCP — принимать подключения</option>
          <option value="tcpConnect">TCP — подключиться к серверу</option>
        </select>
      </div>
      <Field
        label={mode === "tcpConnect" ? "Адрес сервера" : "Адрес и порт"}
        value={address}
        onChange={setAddress}
      />
      <Field label="Лимит, МБ" value={maxMb} onChange={setMaxMb} />
      <div className="note">
        Каждая датаграмма или TCP-сессия становится сегментом; документ растёт по мере поступления
        данных и открыт только для чтения.
      </div>
    </Modal>
  );
}

// ---------------------------------------------------------------- imaging & cloning (DISK-08/09)

function NeedTab() {
  return (
    <Modal title="Нет документа" footer={<CancelBtn label="Закрыть" />}>
      <div className="note">Сначала откройте диск, образ или файл.</div>
    </Modal>
  );
}

function CloneDialog() {
  const tab = useActiveTab();
  const tabs = useTabs((s) => s.tabs);
  const [target, setTarget] = useState<"raw" | "e01" | "doc">("raw");
  const [docId, setDocId] = useState<number | null>(null);
  const [writable, setWritable] = useState<number[]>([]);
  const [verify, setVerify] = useState(true);
  const [meta, setMeta] = useState({
    caseNumber: "",
    evidenceNumber: "",
    description: "",
    examiner: "",
    notes: "",
  });
  const [result, setResult] = useState<string | null>(null);
  const job = useJob();
  useEffect(() => {
    let alive = true;
    Promise.all(tabs.filter((t) => t.id !== tab?.id).map((t) => sourceInfo(t.id))).then((list) => {
      if (!alive) return;
      const ids = tabs
        .filter((t) => t.id !== tab?.id)
        .filter((_, i) => list[i]?.writable && list[i]?.kind === "disk")
        .map((t) => t.id);
      setWritable(ids);
      setDocId((cur) => cur ?? ids[0] ?? null);
    });
    return () => {
      alive = false;
    };
  }, [tabs.length, tab?.id]);
  if (!tab) return <NeedTab />;
  const run = async () => {
    let t: Parameters<typeof src.clone>[1];
    if (target === "doc") {
      if (docId === null) return;
      t = { kind: "doc", id: docId };
    } else {
      const ext = target === "e01" ? "E01" : "img";
      const p = await saveFile({
        defaultPath: `${tab.info.name.replace(/[\\/:]/g, "_")}.${ext}`,
        filters: [{ name: target === "e01" ? "EnCase E01" : "Raw / dd", extensions: [ext] }],
      });
      if (!p) return;
      t = target === "e01" ? { kind: "e01", path: p, meta } : { kind: "raw", path: p };
    }
    setResult(null);
    try {
      const r = await job.run((p, j) => src.clone(tab.id, t, target !== "e01" && verify, p, j));
      if (!r) return;
      const bytes = r.bytesCopied ?? r.bytes ?? 0;
      const v = r.verified == null ? "" : r.verified ? " · проверено" : " · НЕ совпадает!";
      setResult(`Скопировано ${fmtSize(bytes)} · MD5 ${r.md5}${v}`);
      toast("Готово");
    } catch (e) {
      A.report(e);
    }
  };
  return (
    <Modal
      title="Клонировать или снять образ"
      footer={
        <>
          <CancelBtn label={result ? "Закрыть" : "Отмена"} />
          <button
            type="button"
            className={`btn ${target === "doc" ? "danger" : "pri"}`}
            disabled={job.running || (target === "doc" && docId === null)}
            onClick={run}
          >
            {target === "doc" ? "Клонировать" : "Сохранить образ…"}
          </button>
        </>
      }
    >
      <div className="note">
        Источник: «{tab.info.name}», {fmtSize(tab.info.baseLen)} — копируются исходные данные, без
        несохранённых правок. MD5 считается на лету.
      </div>
      <div className="opt-list">
        {(
          [
            ["raw", "Raw-образ (dd)", "побайтная копия, проверка чтением"],
            ["e01", "E01 (EnCase)", "сжатие, метаданные дела, MD5 внутри"],
            ["doc", "На другой диск", "открытый для записи диск-приёмник"],
          ] as const
        ).map(([k, t, n]) => (
          <button
            type="button"
            key={k}
            className={`opt${target === k ? " on" : ""}`}
            onClick={() => setTarget(k)}
          >
            <span />
            <span>
              <b>{t}</b>
              <span className="note"> · {n}</span>
            </span>
            <span />
          </button>
        ))}
      </div>
      {target === "e01" && (
        <>
          <Field
            label="Номер дела"
            value={meta.caseNumber}
            onChange={(v) => setMeta({ ...meta, caseNumber: v })}
          />
          <Field
            label="Номер улики"
            value={meta.evidenceNumber}
            onChange={(v) => setMeta({ ...meta, evidenceNumber: v })}
          />
          <Field
            label="Описание"
            value={meta.description}
            onChange={(v) => setMeta({ ...meta, description: v })}
          />
          <Field
            label="Эксперт"
            value={meta.examiner}
            onChange={(v) => setMeta({ ...meta, examiner: v })}
          />
        </>
      )}
      {target === "doc" &&
        (writable.length ? (
          <div className="row-f">
            <span>Диск-приёмник</span>
            <select
              className="inp"
              value={docId ?? ""}
              onChange={(e) => setDocId(Number(e.target.value))}
            >
              {writable.map((id) => {
                const t = tabs.find((x) => x.id === id);
                return (
                  <option key={id} value={id}>
                    {t?.info.name} · {fmtSize(t?.info.len ?? 0)}
                  </option>
                );
              })}
            </select>
          </div>
        ) : (
          <div className="src-warn">
            Откройте диск-приёмник через «Открыть диск… → Открыть для записи».
          </div>
        ))}
      {target === "doc" && writable.length > 0 && (
        <div className="src-warn">Все данные диска-приёмника будут перезаписаны.</div>
      )}
      {target !== "e01" && (
        <label className="chk">
          <input type="checkbox" checked={verify} onChange={(e) => setVerify(e.target.checked)} />
          Проверить копию повторным чтением
        </label>
      )}
      <JobBar job={job} label="Копирование" />
      {result && <div className="src-fp">{result}</div>}
    </Modal>
  );
}

// ---------------------------------------------------------------- wiping & shredding (DISK-10, FILE-14)

type MethodId = "zeros" | "random" | "dod3Pass" | "gutmann";

const METHODS: { id: MethodId; name: string; note: string }[] = [
  { id: "zeros", name: "Нули", note: "1 проход 0x00" },
  { id: "random", name: "Случайные данные", note: "1 проход" },
  { id: "dod3Pass", name: "DoD 5220.22-M", note: "3 прохода: 0x00, 0xFF, случайные" },
  { id: "gutmann", name: "Гутман", note: "до 35 проходов" },
];

function MethodPicker({
  method,
  setMethod,
  passes,
  setPasses,
}: {
  method: MethodId;
  setMethod: (m: MethodId) => void;
  passes: string;
  setPasses: (p: string) => void;
}) {
  return (
    <>
      <div className="opt-list">
        {METHODS.map((m) => (
          <button
            type="button"
            key={m.id}
            className={`opt${method === m.id ? " on" : ""}`}
            onClick={() => setMethod(m.id)}
          >
            <span />
            <span>
              <b>{m.name}</b>
              <span className="note"> · {m.note}</span>
            </span>
            <span />
          </button>
        ))}
      </div>
      {method === "gutmann" && (
        <Field label="Проходов (3–35)" value={passes} onChange={setPasses} />
      )}
    </>
  );
}

function wipeMethod(id: MethodId, passes: string): WipeMethod {
  return id === "gutmann"
    ? { method: "gutmann", passes: Math.min(35, Math.max(3, Number(passes) || 35)) }
    : { method: id };
}

function WipeDialog() {
  const tab = useActiveTab();
  const info = useSourceInfo(tab?.id);
  const [method, setMethod] = useState<MethodId>("zeros");
  const [passes, setPasses] = useState("35");
  const [scope, setScope] = useState<"all" | "selection">("all");
  const [verify, setVerify] = useState(true);
  const [confirmText, setConfirmText] = useState("");
  const [result, setResult] = useState<string | null>(null);
  const job = useJob();
  if (!tab) return <NeedTab />;
  const sel = selection(tab);
  const hasSel = sel.end > sel.start;
  const word = "ЗАТЕРЕТЬ";
  const run = async () => {
    const start = scope === "selection" && hasSel ? sel.start : 0;
    const len = scope === "selection" && hasSel ? sel.end - sel.start : null;
    try {
      const r = await job.run((p, j) =>
        src.wipe(tab.id, wipeMethod(method, passes), verify, start, len, p, j),
      );
      if (!r) return;
      setResult(
        `Записано ${fmtSize(r.bytesWritten)} за ${r.passes} прох.${
          r.verified == null ? "" : r.verified ? " · проверка пройдена" : " · проверка НЕ пройдена"
        }`,
      );
      toast("Затирание завершено");
    } catch (e) {
      A.report(e);
    }
  };
  return (
    <Modal
      title="Затереть"
      footer={
        <>
          <CancelBtn label={result ? "Закрыть" : "Отмена"} />
          <button
            type="button"
            className="btn danger"
            disabled={confirmText.trim().toUpperCase() !== word || job.running || !info?.writable}
            onClick={run}
          >
            Затереть
          </button>
        </>
      }
    >
      {!info?.writable && (
        <div className="src-warn">
          «{tab.info.name}» открыт только для чтения. Откройте диск через «Открыть диск… → Открыть
          для записи».
        </div>
      )}
      <MethodPicker method={method} setMethod={setMethod} passes={passes} setPasses={setPasses} />
      <div className="row-f">
        <span>Область</span>
        <select
          className="inp"
          value={scope}
          onChange={(e) => setScope(e.target.value as typeof scope)}
        >
          <option value="all">весь источник · {fmtSize(tab.info.baseLen)}</option>
          <option value="selection" disabled={!hasSel}>
            выделение · {hasSel ? fmtSize(sel.end - sel.start) : "нет"}
          </option>
        </select>
      </div>
      <label className="chk">
        <input type="checkbox" checked={verify} onChange={(e) => setVerify(e.target.checked)} />
        Проверить последний проход чтением
      </label>
      <div className="src-warn">
        Данные «{tab.info.name}» будут уничтожены без возможности восстановления — отмены нет.
        Введите {word}, чтобы подтвердить.
      </div>
      <input
        className="inp"
        aria-label="Подтверждение затирания"
        value={confirmText}
        placeholder={word}
        onChange={(e) => setConfirmText(e.target.value)}
      />
      <JobBar job={job} label="Затирание" />
      {result && <div className="src-fp">{result}</div>}
    </Modal>
  );
}

function ShredDialog({ path: initial }: { path?: string }) {
  const [path, setPath] = useState(initial ?? "");
  const [method, setMethod] = useState<MethodId>("dod3Pass");
  const [passes, setPasses] = useState("35");
  const [rename, setRename] = useState(true);
  const [del, setDel] = useState(true);
  const [confirmText, setConfirmText] = useState("");
  const [result, setResult] = useState<string | null>(null);
  const job = useJob();
  const pick = async () => {
    const p = await openFile({ multiple: false });
    if (typeof p === "string") setPath(p);
  };
  const name = path.split(/[\\/]/).pop() ?? "";
  const run = async () => {
    try {
      const r = await job.run((p, j) =>
        src.shred(path, wipeMethod(method, passes), rename, del, p, j),
      );
      if (!r) return;
      setResult(
        r.deleted
          ? `Уничтожен: ${r.passes} прох. по ${fmtSize(r.bytesPerPass)}`
          : `Перезаписан ${r.passes} раз, файл оставлен: ${r.finalPath}`,
      );
      toast("Файл уничтожен");
    } catch (e) {
      A.report(e);
    }
  };
  return (
    <Modal
      title="Уничтожить файл"
      footer={
        <>
          <CancelBtn label={result ? "Закрыть" : "Отмена"} />
          <button
            type="button"
            className="btn danger"
            disabled={!path || confirmText.trim() !== name || job.running || !!result}
            onClick={run}
          >
            Уничтожить
          </button>
        </>
      }
    >
      <div className="row-f">
        <span>Файл</span>
        <div className="src-inline">
          <input className="inp" aria-label="Файл" value={path} readOnly />
          <button type="button" className="btn" onClick={pick}>
            Выбрать…
          </button>
        </div>
      </div>
      <MethodPicker method={method} setMethod={setMethod} passes={passes} setPasses={setPasses} />
      <label className="chk">
        <input type="checkbox" checked={rename} onChange={(e) => setRename(e.target.checked)} />
        Переименовать в случайное имя перед удалением
      </label>
      <label className="chk">
        <input type="checkbox" checked={del} onChange={(e) => setDel(e.target.checked)} />
        Удалить после перезаписи
      </label>
      <div className="note">
        На SSD и в файловых системах с копированием при записи (APFS, Btrfs) старые блоки могут
        остаться — для них надёжнее шифрование диска.
      </div>
      {path && (
        <>
          <div className="src-warn">Введите имя файла «{name}», чтобы подтвердить.</div>
          <input
            className="inp"
            aria-label="Подтверждение уничтожения"
            value={confirmText}
            onChange={(e) => setConfirmText(e.target.value)}
          />
        </>
      )}
      <JobBar job={job} label="Перезапись" />
      {result && <div className="src-fp">{result}</div>}
    </Modal>
  );
}

// ---------------------------------------------------------------- alternate streams (FILE-17)

const STREAM_KIND: Record<StreamInfo["kind"], string> = {
  extendedAttribute: "xattr",
  resourceFork: "resource fork",
  alternateDataStream: "ADS",
};

function StreamsDialog({ path: initial }: { path?: string }) {
  const tab = useActiveTab();
  const info = useSourceInfo(tab?.id);
  const [path, setPath] = useState(initial ?? tab?.info.path ?? "");
  const [list, setList] = useState<StreamInfo[] | null>(null);
  const load = () => {
    if (!path) return;
    src
      .streams(path)
      .then(setList)
      .catch((e) => {
        setList(null);
        A.report(e);
      });
  };
  useEffect(load, [path]);
  const pick = async () => {
    const p = await openFile({ multiple: false });
    if (typeof p === "string") setPath(p);
  };
  const remove = async (name: string) => {
    try {
      await src.streamRemove(path, name);
      toast(`Поток «${name}» удалён`);
      load();
    } catch (e) {
      A.report(e);
    }
  };
  const saveBack = async () => {
    if (!tab) return;
    try {
      await src.streamSave(tab.id);
      toast("Поток записан в файл");
    } catch (e) {
      A.report(e);
    }
  };
  return (
    <Modal
      title="Потоки файла"
      footer={
        <>
          {info?.kind === "fileStream" && (
            <button type="button" className="btn pri" onClick={saveBack}>
              Записать «{tab?.info.name}» обратно
            </button>
          )}
          <CancelBtn label="Закрыть" />
        </>
      }
    >
      <div className="row-f">
        <span>Файл</span>
        <div className="src-inline">
          <input className="inp" aria-label="Файл" value={path} readOnly />
          <button type="button" className="btn" onClick={pick}>
            Выбрать…
          </button>
        </div>
      </div>
      {list && list.length === 0 && (
        <div className="note">У файла нет расширенных атрибутов, resource fork или ADS.</div>
      )}
      <div className="list src-list">
        {list?.map((s) => (
          <div key={s.name} className="src-row">
            <span className="t">
              <b>{s.name}</b>
              <span>
                {STREAM_KIND[s.kind]} · {fmtSize(s.size)}
              </span>
            </span>
            <span className="src-inline">
              <button
                type="button"
                className="chip"
                onClick={() =>
                  adoptAndClose(src.streamOpen(path, s.name), "Поток открыт в памяти").catch(
                    A.report,
                  )
                }
              >
                Открыть
              </button>
              <button type="button" className="chip" onClick={() => remove(s.name)}>
                Удалить
              </button>
            </span>
          </div>
        ))}
      </div>
    </Modal>
  );
}

// ---------------------------------------------------------------- write-back preview (DISK-12)

function CommitDialog() {
  const tab = useActiveTab();
  const [preview, setPreview] = useState<CommitPreview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    if (!tab) return;
    src
      .commitPreview(tab.id)
      .then(setPreview)
      .catch((e) => setError(e?.message ?? String(e)));
  }, [tab?.id]);
  if (!tab) return <NeedTab />;
  const commit = async () => {
    setBusy(true);
    try {
      const r = await src.commit(tab.id);
      A.applyInfo(r.info);
      closeDialog();
      toast(
        r.journal
          ? `Записано ${fmtSize(r.bytes)} · журнал отката: ${r.journal}`
          : `Записано ${fmtSize(r.bytes)}`,
      );
    } catch (e) {
      A.report(e);
    } finally {
      setBusy(false);
    }
  };
  const rollback = async () => {
    const p = await openFile({
      multiple: false,
      filters: [{ name: "Журнал", extensions: ["json"] }],
    });
    if (typeof p !== "string") return;
    try {
      A.applyInfo(await src.rollback(tab.id, p));
      closeDialog();
      toast("Изменения откачены по журналу");
    } catch (e) {
      A.report(e);
    }
  };
  return (
    <Modal
      title="Записать в источник"
      wide
      footer={
        <>
          <button type="button" className="btn" onClick={rollback}>
            Откатить по журналу…
          </button>
          <div className="grow" />
          <CancelBtn />
          <button
            type="button"
            className="btn danger"
            disabled={!preview || preview.count === 0 || busy}
            onClick={commit}
          >
            Записать {preview ? fmtSize(preview.total) : ""}
          </button>
        </>
      }
    >
      {error && <div className="src-warn">{error}</div>}
      {preview && preview.count === 0 && (
        <div className="note">Несохранённых замен байтов нет.</div>
      )}
      {preview && preview.count > 0 && (
        <>
          <div className="note">
            {preview.count} участков, {fmtSize(preview.total)} будут записаны прямо в «
            {tab.info.name}». Старые байты сохранятся в журнал — по нему изменения можно откатить.
          </div>
          <div className="list src-list">
            {preview.writes.map((w) => (
              <button
                type="button"
                key={w.offset}
                className="src-row"
                onClick={() => A.selectRange(w.offset, w.offset + w.len)}
              >
                <span className="t">
                  <b className="mono">
                    {addr(w.offset)} · {fmtSize(w.len)}
                  </b>
                  <span>
                    <span style={{ color: "var(--mod)" }}>{hx(w.old)}</span> →{" "}
                    <span style={{ color: "var(--ins)" }}>{hx(w.new)}</span>
                    {w.len > w.new.length ? " …" : ""}
                  </span>
                </span>
              </button>
            ))}
          </div>
        </>
      )}
    </Modal>
  );
}

// ---------------------------------------------------------------- RAID (DISK-11)

function RaidDialog() {
  const tabs = useTabs((s) => s.tabs);
  const [count, setCount] = useState(Math.max(2, Math.min(tabs.length, 4)));
  const [members, setMembers] = useState<(number | null)[]>(() =>
    tabs.slice(0, 8).map((t) => t.id),
  );
  const [level, setLevel] = useState<"raid0" | "raid5">("raid0");
  const [layout, setLayout] = useState("leftSymmetric");
  const [stripeKb, setStripeKb] = useState("64");
  const [offset, setOffset] = useState("0");
  const [busy, setBusy] = useState(false);
  const slots = Array.from({ length: count }, (_, i) => members[i] ?? null);
  const missing = slots.filter((m) => m === null).length;
  const ok = level === "raid0" ? missing === 0 : missing <= 1 && count >= 3;
  const run = async () => {
    const config: RaidConfig = {
      level: level === "raid0" ? { level: "raid0" } : { level: "raid5", layout },
      stripeSize: Math.max(1, Number(stripeKb) || 64) * 1024,
      dataOffset: Number(offset) || 0,
    };
    setBusy(true);
    try {
      await adoptAndClose(src.raidOpen(slots, config), "Массив собран (только для чтения)");
    } catch (e) {
      A.report(e);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Modal
      title="Собрать RAID"
      footer={
        <>
          <CancelBtn />
          <button type="button" className="btn pri" disabled={!ok || busy} onClick={run}>
            Собрать
          </button>
        </>
      }
    >
      <div className="row-f">
        <span>Уровень</span>
        <select
          className="inp"
          value={level}
          onChange={(e) => setLevel(e.target.value as typeof level)}
        >
          <option value="raid0">RAID 0 (чередование)</option>
          <option value="raid5">RAID 5 (с чётностью)</option>
        </select>
      </div>
      {level === "raid5" && (
        <div className="row-f">
          <span>Схема чётности</span>
          <select className="inp" value={layout} onChange={(e) => setLayout(e.target.value)}>
            <option value="leftSymmetric">left-symmetric</option>
            <option value="leftAsymmetric">left-asymmetric</option>
            <option value="rightSymmetric">right-symmetric</option>
            <option value="rightAsymmetric">right-asymmetric</option>
          </select>
        </div>
      )}
      <Field label="Размер блока, КБ" value={stripeKb} onChange={setStripeKb} />
      <Field label="Смещение данных, Б" value={offset} onChange={setOffset} />
      <div className="row-f">
        <span>Дисков</span>
        <select className="inp" value={count} onChange={(e) => setCount(Number(e.target.value))}>
          {[2, 3, 4, 5, 6, 7, 8].map((n) => (
            <option key={n} value={n}>
              {n}
            </option>
          ))}
        </select>
      </div>
      {slots.map((m, i) => (
        // biome-ignore lint/suspicious/noArrayIndexKey: slots are positions in the array
        <div className="row-f" key={i}>
          <span>Диск {i + 1}</span>
          <select
            className="inp"
            value={m ?? ""}
            onChange={(e) => {
              const next = [...slots];
              next[i] = e.target.value === "" ? null : Number(e.target.value);
              setMembers(next);
            }}
          >
            <option value="">— отсутствует —</option>
            {tabs.map((t) => (
              <option key={t.id} value={t.id}>
                {t.info.name} · {fmtSize(t.info.len)}
              </option>
            ))}
          </select>
        </div>
      ))}
      <div className="note">
        Диски — открытые вкладки в порядке массива. В RAID 5 один отсутствующий диск
        восстанавливается по чётности.
      </div>
    </Modal>
  );
}

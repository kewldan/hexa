// Transformations of the selection: recipe builder with live preview,
// write-back, inversion, saved recipes and XOR key guessing (XFRM-01…12).
import { open as openFile, save as saveFile } from "@tauri-apps/plugin-dialog";
import { useEffect, useMemo, useState } from "react";
import { api, type Recipe, type Step, type XfrmPreview, type XorGuess } from "../../ipc/api";
import { fmtSize } from "../../lib/format";
import * as A from "../../state/actions";
import { settings, useSettings } from "../../state/settings";
import { hasSelection, selection, type Tab } from "../../state/tabs";
import { toast } from "../../state/ui";
import { Icon } from "../icons";

/** Without a selection the whole file is used up to this size. */
const WHOLE_FILE_LIMIT = 16 << 20;

type Field = {
  key: string;
  label: string;
  type: "num" | "text" | "hex" | "bool" | "sel";
  options?: readonly string[];
};

interface StepDef {
  kind: string;
  title: string;
  group: string;
  make: () => Step;
}

const W = { key: "width", label: "Ширина", type: "sel", options: ["8", "16", "32", "64"] } as const;
const E = { key: "endian", label: "Порядок", type: "sel", options: ["little", "big"] } as const;

export const STEP_DEFS: StepDef[] = [
  {
    kind: "bitwise",
    title: "Логика (XOR/AND/OR…)",
    group: "Биты",
    make: () => ({ kind: "bitwise", op: "xor", operand: "0xFF", width: 8, endian: "little" }),
  },
  { kind: "not", title: "NOT", group: "Биты", make: () => ({ kind: "not" }) },
  {
    kind: "shift",
    title: "Сдвиг",
    group: "Биты",
    make: () => ({ kind: "shift", direction: "left", amount: 1, width: 8, endian: "little" }),
  },
  {
    kind: "rotate",
    title: "Циклический сдвиг",
    group: "Биты",
    make: () => ({ kind: "rotate", direction: "left", amount: 1, width: 8, endian: "little" }),
  },
  {
    kind: "reverseBits",
    title: "Обратить биты",
    group: "Биты",
    make: () => ({ kind: "reverseBits", width: 8 }),
  },
  {
    kind: "arithmetic",
    title: "Арифметика",
    group: "Арифметика",
    make: () => ({
      kind: "arithmetic",
      op: "add",
      operand: "1",
      width: 8,
      endian: "little",
      signed: false,
    }),
  },
  {
    kind: "xorKey",
    title: "XOR с ключом",
    group: "Арифметика",
    make: () => ({ kind: "xorKey", key: "DEADBEEF" }),
  },
  {
    kind: "byteSwap",
    title: "Порядок байт",
    group: "Порядок",
    make: () => ({ kind: "byteSwap", width: 32 }),
  },
  { kind: "reverse", title: "Развернуть", group: "Порядок", make: () => ({ kind: "reverse" }) },
  {
    kind: "interleave",
    title: "Склеить потоки",
    group: "Порядок",
    make: () => ({ kind: "interleave", streams: 2, width: 1 }),
  },
  {
    kind: "deinterleave",
    title: "Разделить на потоки",
    group: "Порядок",
    make: () => ({ kind: "deinterleave", streams: 2, width: 1 }),
  },
  {
    kind: "encode",
    title: "Кодировать",
    group: "Кодировки",
    make: () => ({ kind: "encode", codec: { kind: "base64" } }),
  },
  {
    kind: "decode",
    title: "Декодировать",
    group: "Кодировки",
    make: () => ({ kind: "decode", codec: { kind: "base64" } }),
  },
  {
    kind: "recode",
    title: "Перекодировать текст",
    group: "Кодировки",
    make: () => ({ kind: "recode", from: "windows-1251", to: "utf-8" }),
  },
  {
    kind: "compress",
    title: "Сжать",
    group: "Сжатие",
    make: () => ({ kind: "compress", codec: { kind: "zlib" } }),
  },
  {
    kind: "decompress",
    title: "Распаковать",
    group: "Сжатие",
    make: () => ({ kind: "decompress", codec: { kind: "zlib" } }),
  },
  {
    kind: "encrypt",
    title: "Зашифровать",
    group: "Шифры",
    make: () => ({ kind: "encrypt", cipher: defaultCipher("aes") }),
  },
  {
    kind: "decrypt",
    title: "Расшифровать",
    group: "Шифры",
    make: () => ({ kind: "decrypt", cipher: defaultCipher("aes") }),
  },
];

const CODECS = [
  "base16",
  "base32",
  "base58",
  "base64",
  "ascii85",
  "z85",
  "url",
  "quotedPrintable",
  "uuencode",
  "rot13",
];
const COMPRESSIONS = [
  "deflate",
  "zlib",
  "gzip",
  "bzip2",
  "lzma",
  "xz",
  "lz4",
  "lz4Block",
  "zstd",
  "brotli",
  "lzo",
  "snappy",
  "snappyFramed",
];
const CIPHERS = ["aes", "des", "tripleDes", "blowfish", "rc4", "chaCha20"];
const KEY_LEN: Record<string, number> = {
  aes: 16,
  des: 8,
  tripleDes: 24,
  blowfish: 16,
  rc4: 16,
  chaCha20: 32,
};

function defaultCipher(kind: string): Record<string, unknown> {
  const key = "00".repeat(KEY_LEN[kind] ?? 16);
  if (kind === "rc4") return { kind, key };
  if (kind === "chaCha20") return { kind, key, nonce: "00".repeat(12), counter: 0 };
  return { kind, mode: "cbc", key, iv: "00".repeat(kind === "aes" ? 16 : 8), padding: "pkcs7" };
}

function fieldsOf(step: Step): Field[] {
  switch (step.kind) {
    case "bitwise":
      return [
        { key: "op", label: "Операция", type: "sel", options: ["xor", "and", "or", "nand", "nor"] },
        { key: "operand", label: "Операнд", type: "text" },
        W,
        E,
      ];
    case "shift":
    case "rotate":
      return [
        { key: "direction", label: "Куда", type: "sel", options: ["left", "right"] },
        { key: "amount", label: "На бит", type: "num" },
        W,
        E,
      ];
    case "reverseBits":
      return [W];
    case "arithmetic":
      return [
        { key: "op", label: "Операция", type: "sel", options: ["add", "sub", "mul", "div", "mod"] },
        { key: "operand", label: "Операнд", type: "text" },
        W,
        E,
        { key: "signed", label: "Знаковое", type: "bool" },
      ];
    case "xorKey":
      return [{ key: "key", label: "Ключ (hex)", type: "hex" }];
    case "byteSwap":
      return [{ key: "width", label: "Ширина", type: "sel", options: ["16", "32", "64"] }];
    case "interleave":
    case "deinterleave":
      return [
        { key: "streams", label: "Потоков", type: "num" },
        { key: "width", label: "Элемент, байт", type: "num" },
      ];
    case "recode":
      return [
        { key: "from", label: "Из", type: "text" },
        { key: "to", label: "В", type: "text" },
      ];
    default:
      return [];
  }
}

function sub(obj: Record<string, unknown>): Field[] {
  switch (obj.kind) {
    case "deflate":
    case "zlib":
    case "gzip":
    case "bzip2":
    case "zstd":
      return [{ key: "level", label: "Уровень", type: "num" }];
    case "lzma":
    case "xz":
      return [{ key: "preset", label: "Пресет", type: "num" }];
    case "brotli":
      return [{ key: "quality", label: "Качество", type: "num" }];
    case "lz4Block":
      return [{ key: "size", label: "Размер распак.", type: "num" }];
    case "base16":
      return [{ key: "uppercase", label: "Заглавные", type: "bool" }];
    case "aes":
      return [
        { key: "mode", label: "Режим", type: "sel", options: ["ecb", "cbc", "ctr", "gcm"] },
        { key: "key", label: "Ключ (hex)", type: "hex" },
        { key: "iv", label: "IV (hex)", type: "hex" },
        { key: "padding", label: "Дополнение", type: "sel", options: ["pkcs7", "none"] },
      ];
    case "des":
    case "tripleDes":
    case "blowfish":
      return [
        { key: "mode", label: "Режим", type: "sel", options: ["ecb", "cbc", "ctr"] },
        { key: "key", label: "Ключ (hex)", type: "hex" },
        { key: "iv", label: "IV (hex)", type: "hex" },
        { key: "padding", label: "Дополнение", type: "sel", options: ["pkcs7", "none"] },
      ];
    case "rc4":
      return [{ key: "key", label: "Ключ (hex)", type: "hex" }];
    case "chaCha20":
      return [
        { key: "key", label: "Ключ (hex)", type: "hex" },
        { key: "nonce", label: "Nonce (hex)", type: "hex" },
        { key: "counter", label: "Счётчик", type: "num" },
      ];
    default:
      return [];
  }
}

function FieldInput({
  f,
  value,
  onChange,
}: {
  f: Field;
  value: unknown;
  onChange: (v: unknown) => void;
}) {
  const stop = (e: React.KeyboardEvent) => e.stopPropagation();
  if (f.type === "bool")
    return (
      <label className="chk">
        <input
          type="checkbox"
          checked={Boolean(value)}
          onChange={(e) => onChange(e.target.checked)}
        />
        {f.label}
      </label>
    );
  return (
    <div className="xf-f">
      <span>{f.label}</span>
      {f.type === "sel" ? (
        <select
          aria-label={f.label}
          className="sel-input"
          value={String(value ?? f.options?.[0])}
          onChange={(e) => onChange(f.key === "width" ? Number(e.target.value) : e.target.value)}
        >
          {f.options?.map((o) => (
            <option key={o}>{o}</option>
          ))}
        </select>
      ) : (
        <input
          aria-label={f.label}
          className={`inp${f.type === "hex" ? " mono" : ""}`}
          value={String(value ?? "")}
          inputMode={f.type === "num" ? "numeric" : undefined}
          onKeyDown={stop}
          onChange={(e) =>
            onChange(f.type === "num" ? Number(e.target.value) || 0 : e.target.value)
          }
        />
      )}
    </div>
  );
}

function StepCard({
  step,
  index,
  count,
  onChange,
  onRemove,
  onMove,
}: {
  step: Step;
  index: number;
  count: number;
  onChange: (s: Step) => void;
  onRemove: () => void;
  onMove: (d: -1 | 1) => void;
}) {
  const def = STEP_DEFS.find((d) => d.kind === step.kind);
  const nestedKey = "codec" in step ? "codec" : "cipher" in step ? "cipher" : null;
  const nested = nestedKey ? (step[nestedKey] as Record<string, unknown>) : null;
  const nestedOptions =
    nestedKey === "cipher" ? CIPHERS : step.kind.includes("ompress") ? COMPRESSIONS : CODECS;
  return (
    <div className="xf-step" style={{ ["--delay" as string]: `${index * 0.03}s` }}>
      <div className="xf-h">
        <span className="xf-n">{index + 1}</span>
        <b>{def?.title ?? step.kind}</b>
        <div className="grow" />
        <button
          type="button"
          className="ib"
          aria-label="Выше"
          disabled={index === 0}
          onClick={() => onMove(-1)}
        >
          ↑
        </button>
        <button
          type="button"
          className="ib"
          aria-label="Ниже"
          disabled={index === count - 1}
          onClick={() => onMove(1)}
        >
          ↓
        </button>
        <button type="button" className="ib" aria-label="Удалить шаг" onClick={onRemove}>
          <Icon.close />
        </button>
      </div>
      <div className="xf-body">
        {fieldsOf(step).map((f) => (
          <FieldInput
            key={f.key}
            f={f}
            value={step[f.key]}
            onChange={(v) => onChange({ ...step, [f.key]: v })}
          />
        ))}
        {nested && nestedKey && (
          <>
            <label className="xf-f">
              <span>{nestedKey === "cipher" ? "Шифр" : "Формат"}</span>
              <select
                className="sel-input"
                value={String(nested.kind)}
                onChange={(e) =>
                  onChange({
                    ...step,
                    [nestedKey]:
                      nestedKey === "cipher"
                        ? defaultCipher(e.target.value)
                        : { kind: e.target.value },
                  })
                }
              >
                {nestedOptions.map((o) => (
                  <option key={o}>{o}</option>
                ))}
              </select>
            </label>
            {sub(nested).map((f) => (
              <FieldInput
                key={f.key}
                f={f}
                value={nested[f.key]}
                onChange={(v) => onChange({ ...step, [nestedKey]: { ...nested, [f.key]: v } })}
              />
            ))}
          </>
        )}
      </div>
    </div>
  );
}

function hexLines(bytes: number[], max = 256): string {
  const out: string[] = [];
  for (let i = 0; i < Math.min(bytes.length, max); i += 16) {
    const row = bytes.slice(i, i + 16);
    const hex = row.map((b) => b.toString(16).padStart(2, "0")).join(" ");
    const txt = row.map((b) => (b >= 0x20 && b < 0x7f ? String.fromCharCode(b) : "·")).join("");
    out.push(`${i.toString(16).padStart(4, "0")}  ${hex.padEnd(47)}  ${txt}`);
  }
  return out.join("\n");
}

export function TransformPanel({ tab }: { tab: Tab }) {
  const [steps, setSteps] = useState<Step[]>([]);
  const [preview, setPreview] = useState<XfrmPreview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [xor, setXor] = useState<XorGuess | null>(null);
  const recipes = useSettings((s) => s.s.recipes);
  const range = useMemo(() => {
    if (hasSelection(tab)) return selection(tab);
    return tab.info.len <= WHOLE_FILE_LIMIT ? { start: 0, end: tab.info.len } : null;
  }, [tab.cursor, tab.anchor, tab.info.len, tab]);
  const recipe: Recipe = { steps };

  useEffect(() => {
    if (!range || steps.length === 0) {
      setPreview(null);
      setError(null);
      return;
    }
    const t = setTimeout(() => {
      api
        .xfrmPreview(tab.id, range, { steps }, 512)
        .then((p) => {
          setPreview(p);
          setError(null);
        })
        .catch((e) => {
          setPreview(null);
          setError(e?.message ?? String(e));
        });
    }, 150);
    return () => clearTimeout(t);
  }, [steps, range?.start, range?.end, tab.id, tab.info.version]);

  const add = (kind: string) => {
    const def = STEP_DEFS.find((d) => d.kind === kind);
    if (def) setSteps([...steps, def.make()]);
  };
  const run = async (fn: "apply" | "new") => {
    if (!range) return;
    try {
      if (fn === "apply") {
        const r = await api.xfrmApply(tab.id, range, recipe);
        const len = r.change ? r.change.inserted : range.end - range.start;
        A.applyResult(tab.id, r, range.start + Math.max(0, len - 1), range.start);
        toast("Преобразование применено");
      } else {
        await A.adopt(await api.xfrmToNew(tab.id, range, recipe));
      }
    } catch (e) {
      A.report(e);
    }
  };
  const invert = async () => {
    try {
      setSteps((await api.xfrmInvert(recipe)).steps);
    } catch (e) {
      A.report(e);
    }
  };
  const saveRecipe = () => {
    const name = window.prompt("Название рецепта", steps.map((s) => s.kind).join(" → "));
    if (!name) return;
    const s = settings();
    useSettings
      .getState()
      .set({ recipes: [...s.recipes.filter((r) => r.name !== name), { name, steps }] });
  };
  const exportRecipe = async () => {
    const path = await saveFile({
      defaultPath: "recipe.json",
      filters: [{ name: "JSON", extensions: ["json"] }],
    });
    if (path) await api.writeTextFile(path, JSON.stringify(recipe, null, 2)).catch(A.report);
  };
  const importRecipe = async () => {
    const path = await openFile({
      multiple: false,
      filters: [{ name: "JSON", extensions: ["json"] }],
    });
    if (typeof path !== "string") return;
    try {
      const r = JSON.parse(await api.readTextFile(path)) as Recipe;
      if (!Array.isArray(r.steps)) throw new Error("not a recipe");
      setSteps(r.steps);
    } catch (e) {
      A.report(e);
    }
  };
  const guess = async () => {
    if (!range) return;
    try {
      setXor(await api.xfrmXorGuess(tab.id, range));
    } catch (e) {
      A.report(e);
    }
  };

  const groups = [...new Set(STEP_DEFS.map((d) => d.group))];
  return (
    <>
      <div className="ph">
        <span className="cap">Преобразования</span>
        <span className="tag">{range ? fmtSize(range.end - range.start) : "нет выделения"}</span>
        <div className="grow" />
        <select
          className="sel-input"
          aria-label="Добавить шаг"
          value=""
          onChange={(e) => {
            add(e.target.value);
            e.target.value = "";
          }}
        >
          <option value="">+ шаг</option>
          {groups.map((g) => (
            <optgroup key={g} label={g}>
              {STEP_DEFS.filter((d) => d.group === g).map((d) => (
                <option key={d.kind} value={d.kind}>
                  {d.title}
                </option>
              ))}
            </optgroup>
          ))}
        </select>
      </div>
      {recipes.length > 0 && (
        <div className="xf-saved">
          {recipes.map((r) => (
            <button
              type="button"
              key={r.name}
              className="chip"
              onClick={() => setSteps(r.steps)}
              onContextMenu={(e) => {
                e.preventDefault();
                useSettings
                  .getState()
                  .set({ recipes: settings().recipes.filter((x) => x.name !== r.name) });
              }}
              title="Правый клик — удалить"
            >
              {r.name}
            </button>
          ))}
        </div>
      )}
      {steps.length === 0 ? (
        <div className="empty">
          <span>
            Соберите рецепт из шагов: XOR, сдвиги, Base64, zlib, AES… Превью обновляется на лету,
            результат можно записать на место выделения или открыть в новой вкладке.
          </span>
        </div>
      ) : (
        <div className="xf-steps">
          {steps.map((s, i) => (
            <StepCard
              // biome-ignore lint/suspicious/noArrayIndexKey: steps are positional
              key={i}
              step={s}
              index={i}
              count={steps.length}
              onChange={(n) => setSteps(steps.map((x, j) => (j === i ? n : x)))}
              onRemove={() => setSteps(steps.filter((_, j) => j !== i))}
              onMove={(d) => {
                const n = [...steps];
                [n[i], n[i + d]] = [n[i + d], n[i]];
                setSteps(n);
              }}
            />
          ))}
        </div>
      )}
      {steps.length > 0 && (
        <>
          <div className="xf-prev">
            <div className="xf-ph">
              <span className="cap">Превью</span>
              {preview && (
                <span className="note">
                  {preview.totalLen !== null
                    ? fmtSize(preview.totalLen)
                    : `≥ ${fmtSize(preview.output.length)}`}
                </span>
              )}
            </div>
            {error ? (
              <div className="xf-err">{error}</div>
            ) : (
              <pre className="xf-hex">{preview ? hexLines(preview.output) : "…"}</pre>
            )}
          </div>
          <div className="xf-actions">
            <button
              type="button"
              className="btn pri"
              disabled={!range || !!error}
              onClick={() => run("apply")}
            >
              Заменить выделение
            </button>
            <button
              type="button"
              className="btn"
              disabled={!range || !!error}
              onClick={() => run("new")}
            >
              В новую вкладку
            </button>
          </div>
          <div className="xf-actions">
            <button
              type="button"
              className="chip"
              onClick={invert}
              title="Рецепт, который отменяет этот (XFRM-09)"
            >
              Обратить
            </button>
            <button type="button" className="chip" onClick={saveRecipe}>
              Сохранить
            </button>
            <button type="button" className="chip" onClick={exportRecipe}>
              Экспорт
            </button>
            <button type="button" className="chip" onClick={() => setSteps([])}>
              Очистить
            </button>
          </div>
        </>
      )}
      <div className="xf-actions">
        <button type="button" className="chip" onClick={importRecipe}>
          Импорт рецепта
        </button>
        <button type="button" className="chip" onClick={guess} disabled={!range}>
          Подобрать XOR-ключ
        </button>
      </div>
      {xor && (
        <div className="xf-xor">
          <div className="cap">Кандидаты ключей</div>
          {[...xor.single.slice(0, 3), ...xor.repeating.slice(0, 5)].map((c, i) => (
            <button
              type="button"
              // biome-ignore lint/suspicious/noArrayIndexKey: ranked list
              key={i}
              className="li"
              style={{ gridTemplateColumns: "minmax(0,1fr) auto" }}
              onClick={() => setSteps([{ kind: "xorKey", key: c.key }])}
            >
              <span className="mono">{c.key.length > 24 ? `${c.key.slice(0, 24)}…` : c.key}</span>
              <span className="note">
                {Math.round(c.score * 100)}% · {c.method === "text" ? "текст" : "нули"}
              </span>
            </button>
          ))}
          {xor.sizes.length > 0 && (
            <div className="note">
              Вероятная длина ключа:{" "}
              {xor.sizes
                .slice(0, 3)
                .map((s) => s.keySize)
                .join(", ")}
            </div>
          )}
        </div>
      )}
    </>
  );
}

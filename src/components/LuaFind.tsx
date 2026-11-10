// Options of the «Lua» kind of the find bar (SRCH-08).
import { useConsole } from "../lib/script";

export const LUA_PLACEHOLDER = "function(off, b) return b:byte(1) == 0x4D end";

export function LuaFindOptions() {
  const window = useConsole((c) => c.luaWindow);
  return (
    <select
      className="sel-input"
      value={window}
      aria-label="Окно предиката"
      title="Сколько байт получает предикат вторым аргументом"
      onChange={(e) => useConsole.getState().set({ luaWindow: Number(e.target.value) })}
    >
      {[1, 2, 4, 8, 16, 32, 64, 256].map((n) => (
        <option key={n} value={n}>
          окно {n} Б
        </option>
      ))}
    </select>
  );
}

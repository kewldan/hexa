// Applies requests of running scripts to this window and answers `hexa.prompt`
// (SCR-01, SCR-09).
import { useEffect, useRef, useState } from "react";
import { answerPrompt, installScriptBridge, type PendingPrompt, useConsole } from "../lib/script";
import { Modal } from "./dialogs/Dialogs";
import "../styles/script.css";

function PromptDialog({ prompt }: { prompt: PendingPrompt }) {
  const [value, setValue] = useState(prompt.default);
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => {
    input.current?.focus();
    input.current?.select();
  }, []);
  return (
    <Modal
      title={prompt.title || "Скрипт"}
      onClose={() => answerPrompt(null)}
      footer={
        <>
          <button type="button" className="btn" onClick={() => answerPrompt(null)}>
            Отмена
          </button>
          <button type="button" className="btn pri" onClick={() => answerPrompt(value)}>
            OK
          </button>
        </>
      }
    >
      <label className="sc-prompt">
        <span>{prompt.message}</span>
        <input
          className="inp"
          ref={input}
          value={value}
          onChange={(e) => setValue(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") answerPrompt(value);
          }}
        />
      </label>
    </Modal>
  );
}

export function ScriptBridge() {
  const prompt = useConsole((c) => c.prompt);
  useEffect(() => installScriptBridge(), []);
  return prompt ? <PromptDialog key={prompt.req} prompt={prompt} /> : null;
}

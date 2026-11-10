// Tiny, safe Markdown renderer for annotation notes (NAV-17): everything is
// HTML-escaped first, then a small subset is turned into tags.

function esc(s: string): string {
  return s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
}

function inline(s: string): string {
  return s
    .replace(/`([^`]+)`/g, "<code>$1</code>")
    .replace(/\*\*([^*]+)\*\*/g, "<b>$1</b>")
    .replace(/\*([^*]+)\*/g, "<i>$1</i>")
    .replace(
      /\[([^\]]+)\]\((https?:\/\/[^)\s]+)\)/g,
      '<a href="$2" target="_blank" rel="noreferrer">$1</a>',
    );
}

export function renderMarkdown(src: string): string {
  const out: string[] = [];
  let list = false;
  let code = false;
  for (const raw of esc(src).split("\n")) {
    if (raw.startsWith("```")) {
      out.push(code ? "</pre>" : "<pre>");
      code = !code;
      continue;
    }
    if (code) {
      out.push(`${raw}\n`);
      continue;
    }
    const li = /^\s*[-*] (.*)$/.exec(raw);
    if (li) {
      if (!list) out.push("<ul>");
      list = true;
      out.push(`<li>${inline(li[1])}</li>`);
      continue;
    }
    if (list) {
      out.push("</ul>");
      list = false;
    }
    const h = /^(#{1,3}) (.*)$/.exec(raw);
    if (h) out.push(`<h${h[1].length + 2}>${inline(h[2])}</h${h[1].length + 2}>`);
    else if (raw.trim()) out.push(`<p>${inline(raw)}</p>`);
  }
  if (list) out.push("</ul>");
  if (code) out.push("</pre>");
  return out.join("");
}

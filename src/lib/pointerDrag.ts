// Pointer-based drag and drop for tabs and panels. HTML5 drag and drop does
// not coexist with native file drops in WebView2 (Windows), so we do it by hand.

export interface PointerDragOptions {
  /** Called once the pointer moved far enough to count as a drag. */
  onStart?: () => void;
  onMove?: (e: PointerEvent, over: Element | null) => void;
  onDrop: (e: PointerEvent, over: Element | null, outside: boolean) => void;
  onCancel?: () => void;
  threshold?: number;
}

export function startPointerDrag(down: React.PointerEvent, opts: PointerDragOptions) {
  if (down.button !== 0) return;
  const x0 = down.clientX;
  const y0 = down.clientY;
  const threshold = opts.threshold ?? 5;
  let dragging = false;
  const move = (e: PointerEvent) => {
    if (!dragging) {
      if (Math.hypot(e.clientX - x0, e.clientY - y0) < threshold) return;
      dragging = true;
      document.body.style.cursor = "grabbing";
      opts.onStart?.();
    }
    opts.onMove?.(e, document.elementFromPoint(e.clientX, e.clientY));
  };
  const up = (e: PointerEvent) => {
    window.removeEventListener("pointermove", move);
    window.removeEventListener("pointerup", up);
    window.removeEventListener("keydown", esc, true);
    document.body.style.cursor = "";
    if (!dragging) return;
    const outside =
      e.clientX < 0 ||
      e.clientY < 0 ||
      e.clientX > window.innerWidth ||
      e.clientY > window.innerHeight;
    opts.onDrop(e, document.elementFromPoint(e.clientX, e.clientY), outside);
  };
  const esc = (e: KeyboardEvent) => {
    if (e.key !== "Escape") return;
    e.stopPropagation();
    window.removeEventListener("pointermove", move);
    window.removeEventListener("pointerup", up);
    window.removeEventListener("keydown", esc, true);
    document.body.style.cursor = "";
    opts.onCancel?.();
  };
  window.addEventListener("pointermove", move);
  window.addEventListener("pointerup", up);
  window.addEventListener("keydown", esc, true);
  // Mouse events keep arriving outside the window while the button is held,
  // which is what tab tear-off relies on.
}

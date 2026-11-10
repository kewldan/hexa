// Stroke icons from the Hexa mockups (16×16, currentColor).
import type { ReactNode } from "react";

function I({ children, size }: { children: ReactNode; size?: number }) {
  return (
    <svg
      className="i"
      viewBox="0 0 16 16"
      aria-hidden="true"
      style={size ? { width: size, height: size } : undefined}
    >
      {children}
    </svg>
  );
}

export const Icon = {
  sidebarL: () => (
    <I>
      <rect x="1.8" y="2.8" width="12.4" height="10.4" rx="2" />
      <path d="M6 2.8v10.4" />
    </I>
  ),
  sidebarR: () => (
    <I>
      <rect x="1.8" y="2.8" width="12.4" height="10.4" rx="2" />
      <path d="M10 2.8v10.4" />
    </I>
  ),
  plus: () => (
    <I>
      <path d="M8 3.5v9M3.5 8h9" />
    </I>
  ),
  goto: () => (
    <I>
      <path d="M2.5 8h8M7.5 4.5L11 8l-3.5 3.5M13.5 3v10" />
    </I>
  ),
  find: () => (
    <I>
      <path d="M2.5 4h4M2.5 8h3M2.5 12h4" />
      <circle cx="10.5" cy="8" r="2.6" />
      <path d="M12.4 9.9l1.6 1.6" />
    </I>
  ),
  search: () => (
    <I>
      <circle cx="7" cy="7" r="4.2" />
      <path d="M10.2 10.2l3.3 3.3" />
    </I>
  ),
  compare: () => (
    <I>
      <rect x="1.8" y="2.5" width="5" height="11" rx="1.4" />
      <rect x="9.2" y="2.5" width="5" height="11" rx="1.4" />
      <path d="M3.6 6h1.4M11 6h1.4M11 9h1.4" />
    </I>
  ),
  up: () => (
    <I>
      <path d="M4 10l4-4 4 4" />
    </I>
  ),
  down: () => (
    <I>
      <path d="M4 6l4 4 4-4" />
    </I>
  ),
  close: () => (
    <I>
      <path d="M4.5 4.5l7 7M11.5 4.5l-7 7" />
    </I>
  ),
  sun: () => (
    <I>
      <circle cx="8" cy="8" r="3" />
      <path d="M8 1.5v1.6M8 12.9v1.6M1.5 8h1.6M12.9 8h1.6M3.4 3.4l1.1 1.1M11.5 11.5l1.1 1.1M3.4 12.6l1.1-1.1M11.5 4.5l1.1-1.1" />
    </I>
  ),
  moon: () => (
    <I>
      <path d="M13.2 9.6A5.6 5.6 0 0 1 6.4 2.8a5.6 5.6 0 1 0 6.8 6.8z" />
    </I>
  ),
  contrast: () => (
    <I>
      <circle cx="8" cy="8" r="5.6" />
      <path d="M8 2.4v11.2" />
      <path d="M8 2.4a5.6 5.6 0 0 1 0 11.2z" fill="currentColor" />
    </I>
  ),
  check: () => (
    <I>
      <circle cx="8" cy="8" r="6.2" />
      <path d="M5.4 8.2l1.8 1.8 3.4-3.6" />
    </I>
  ),
  alert: () => (
    <I>
      <circle cx="8" cy="8" r="6.2" />
      <path d="M8 4.8v3.8M8 11h.01" />
    </I>
  ),
  star: () => (
    <I>
      <path d="M8 1.9l1.85 3.75 4.15.6-3 2.92.7 4.13L8 11.35 4.3 13.3l.7-4.13-3-2.92 4.15-.6z" />
    </I>
  ),
  newFile: () => (
    <I>
      <path d="M9.5 1.8H4.3a1.5 1.5 0 0 0-1.5 1.5v9.4a1.5 1.5 0 0 0 1.5 1.5h7.4a1.5 1.5 0 0 0 1.5-1.5V5.5z" />
      <path d="M9.5 1.8v3.7h3.7M8 8v4M6 10h4" />
    </I>
  ),
  disk: () => (
    <I>
      <rect x="2" y="3.5" width="12" height="9" rx="2" />
      <path d="M4.5 9.5h.01M7 9.5h4.5" />
    </I>
  ),
  chip: () => (
    <I>
      <rect x="4" y="4" width="8" height="8" rx="1.5" />
      <path d="M6.5 1.8V4M9.5 1.8V4M6.5 12v2.2M9.5 12v2.2M1.8 6.5H4M1.8 9.5H4M12 6.5h2.2M12 9.5h2.2" />
    </I>
  ),
  lock: () => (
    <I>
      <rect x="3.5" y="7" width="9" height="6.5" rx="1.5" />
      <path d="M5.5 7V5a2.5 2.5 0 0 1 5 0v2" />
    </I>
  ),
  clock: () => (
    <I>
      <circle cx="8" cy="8" r="6.2" />
      <path d="M8 4.5V8l2.4 1.6" />
    </I>
  ),
  hash: () => (
    <I>
      <path d="M6 2.5l-1.5 11M11.5 2.5l-1.5 11M3 6h10.5M2.5 10H13" />
    </I>
  ),
  list: () => (
    <I>
      <path d="M5.5 4h8M5.5 8h8M5.5 12h8M2.5 4h.01M2.5 8h.01M2.5 12h.01" />
    </I>
  ),
  layers: () => (
    <I>
      <path d="M8 2.5l5.5 3L8 8.5l-5.5-3zM2.5 8.5l5.5 3 5.5-3M2.5 11.2l5.5 3 5.5-3" />
    </I>
  ),
  refresh: () => (
    <I>
      <path d="M13 8a5 5 0 1 1-1.5-3.6M13 2.8v2.6h-2.6" />
    </I>
  ),
  copy: () => (
    <I>
      <rect x="5" y="5" width="8.5" height="8.5" rx="1.6" />
      <path d="M11 5V3.8A1.3 1.3 0 0 0 9.7 2.5H3.8a1.3 1.3 0 0 0-1.3 1.3v5.9A1.3 1.3 0 0 0 3.8 11H5" />
    </I>
  ),
  gear: () => (
    <I>
      <circle cx="8" cy="8" r="2.2" />
      <path d="M8 1.8v1.6M8 12.6v1.6M3.6 3.6l1.1 1.1M11.3 11.3l1.1 1.1M1.8 8h1.6M12.6 8h1.6M3.6 12.4l1.1-1.1M11.3 4.7l1.1-1.1" />
    </I>
  ),
  upload: () => (
    <svg className="i" viewBox="0 0 24 24" aria-hidden="true">
      <path d="M12 15V4M7.5 8.5L12 4l4.5 4.5" />
      <path d="M4 14v3.5A2.5 2.5 0 0 0 6.5 20h11a2.5 2.5 0 0 0 2.5-2.5V14" />
    </svg>
  ),
  window: () => (
    <I>
      <rect x="1.8" y="3" width="10" height="9" rx="1.6" />
      <path d="M5 13.5h7.5a1.7 1.7 0 0 0 1.7-1.7V6" />
    </I>
  ),
  stop: () => (
    <I>
      <rect x="4" y="4" width="8" height="8" rx="1.5" />
    </I>
  ),
};

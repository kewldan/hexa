// Colour-vision-safe highlight colours (UI-18): hues used for marks, diff,
// analysis and structure layers map to the Okabe–Ito palette, which stays
// distinguishable with protanopia, deuteranopia and tritanopia.

const OKABE_ITO = [
  "#E69F00",
  "#56B4E9",
  "#009E73",
  "#F0E442",
  "#0072B2",
  "#D55E00",
  "#CC79A7",
  "#000000",
];

function hue(hex: string): number | null {
  const m = /^#?([0-9a-f]{6})/i.exec(hex);
  if (!m) return null;
  const n = Number.parseInt(m[1], 16);
  const r = ((n >> 16) & 255) / 255;
  const g = ((n >> 8) & 255) / 255;
  const b = (n & 255) / 255;
  const max = Math.max(r, g, b);
  const d = max - Math.min(r, g, b);
  if (d < 0.08) return null; // grey: keep
  const h = max === r ? ((g - b) / d) % 6 : max === g ? (b - r) / d + 2 : (r - g) / d + 4;
  return (h * 60 + 360) % 360;
}

/** Maps a colour to the nearest safe hue; greys and non-hex values pass through. */
export function safeColor(c: string): string {
  const h = hue(c);
  if (h === null) return c;
  const alpha = c.length === 9 ? c.slice(7) : "";
  // red→vermillion, orange→orange, yellow, green→bluish green, cyan→sky, blue, purple→pink
  const pick =
    h < 20 || h >= 330 ? 5 : h < 45 ? 0 : h < 70 ? 3 : h < 165 ? 2 : h < 200 ? 1 : h < 260 ? 4 : 6;
  return OKABE_ITO[pick] + alpha;
}

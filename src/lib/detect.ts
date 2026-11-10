// File format badge from the first bytes (start screen, tabs).

const SIGS: [number[] | string, string, string][] = [
  [[0x89, 0x50, 0x4e, 0x47], "PNG", "Изображение PNG"],
  [[0xff, 0xd8, 0xff], "JPEG", "Изображение JPEG"],
  ["GIF8", "GIF", "Изображение GIF"],
  ["%PDF", "PDF", "Документ PDF"],
  [[0x7f, 0x45, 0x4c, 0x46], "ELF", "Исполняемый файл ELF"],
  [[0xcf, 0xfa, 0xed, 0xfe], "MACH-O", "Mach-O 64-bit"],
  [[0xce, 0xfa, 0xed, 0xfe], "MACH-O", "Mach-O 32-bit"],
  [[0xfe, 0xed, 0xfa, 0xcf], "MACH-O", "Mach-O 64-bit (BE)"],
  [[0xca, 0xfe, 0xba, 0xbe], "FAT", "Universal Mach-O / Java class"],
  ["MZ", "PE", "Исполняемый файл Windows"],
  [[0x1f, 0x8b], "GZIP", "Архив gzip"],
  ["SQLite format 3", "SQLITE", "База SQLite"],
  [[0x00, 0x61, 0x73, 0x6d], "WASM", "Модуль WebAssembly"],
  [[0x37, 0x7a, 0xbc, 0xaf], "7Z", "Архив 7-Zip"],
  ["Rar!", "RAR", "Архив RAR"],
  [[0x50, 0x4b, 0x03, 0x04], "ZIP", "Архив ZIP"],
  ["bplist", "PLIST", "Бинарный plist"],
  ["ID3", "MP3", "Аудио MP3"],
  ["RIFF", "RIFF", "RIFF (WAV/AVI/WebP)"],
  ["OggS", "OGG", "Ogg"],
  ["fLaC", "FLAC", "Аудио FLAC"],
  [[0x28, 0xb5, 0x2f, 0xfd], "ZSTD", "Архив zstd"],
  [[0xfd, 0x37, 0x7a, 0x58, 0x5a], "XZ", "Архив xz"],
  ["BZh", "BZIP2", "Архив bzip2"],
  ["dex\n", "DEX", "Android DEX"],
  ["BM", "BMP", "Изображение BMP"],
];

function matches(head: ArrayLike<number>, sig: number[] | string): boolean {
  const bytes = typeof sig === "string" ? Array.from(sig, (c) => c.charCodeAt(0)) : sig;
  if (head.length < bytes.length) return false;
  return bytes.every((b, i) => head[i] === b);
}

export function detectFormat(
  head: ArrayLike<number>,
  name = "",
): { badge: string; description: string } {
  for (const [sig, badge, description] of SIGS) {
    if (matches(head, sig)) return { badge, description };
  }
  const ext = name.includes(".") ? (name.split(".").pop() ?? "") : "";
  if (ext && ext.length <= 5) return { badge: ext.toUpperCase(), description: "Файл" };
  return { badge: "BIN", description: "Двоичные данные" };
}

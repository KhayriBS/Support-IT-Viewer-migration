/** Format an elapsed duration in ms as "Xh Ymin" or "Y min", "-" if null/zero. */
export function rdFormatDuration(ms: number | null): string {
  if (!ms || ms <= 0) return "-";
  const total = Math.floor(ms / 1000);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  if (h > 0) return `${h}h ${m}min`;
  return `${m} min`;
}

/** Format an ISO datetime string as "HH:MM" using the user's locale. */
export function rdFormatTime(iso: string): string {
  try {
    return new Date(iso).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  } catch {
    return "-";
  }
}

/** Render "il y a X min/h/j" from an absolute epoch ms. */
export function rdFormatRelative(ms: number): string {
  const diff = Date.now() - ms;
  const m = Math.floor(diff / 60000);
  if (m < 1) return "à l'instant";
  if (m < 60) return `Il y a ${m} min`;
  const h = Math.floor(m / 60);
  if (h < 24) return `Il y a ${h}h`;
  const d = Math.floor(h / 24);
  return `Il y a ${d}j`;
}

/** Compact byte size (handles falsy as 0 B). */
export function rdFormatBytes(b: number): string {
  if (!b || b < 1024) return `${b} B`;
  if (b < 1024 * 1024) return `${(b / 1024).toFixed(1)} KB`;
  if (b < 1024 * 1024 * 1024) return `${(b / 1024 / 1024).toFixed(1)} MB`;
  return `${(b / 1024 / 1024 / 1024).toFixed(1)} GB`;
}

/** Map a file name to its rd-file icon modifier class. */
export function rdFileIconClass(name: string): string {
  const lower = name.toLowerCase();
  if (lower.endsWith(".pdf")) return "rd-file__icon--pdf";
  if (lower.endsWith(".pptx") || lower.endsWith(".ppt")) return "rd-file__icon--ppt";
  if (lower.endsWith(".zip") || lower.endsWith(".rar") || lower.endsWith(".7z")) return "rd-file__icon--zip";
  return "rd-file__icon--pdf";
}

/** Byte size with 2-decimal MB precision (used for file transfer rows). */
export function formatFileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

/** Estimation grossière de la taille décodée d'une chaîne base64 (en KB/MB). */
export function formatBytesApprox(base64Len: number): string {
  const approxBytes = Math.floor((base64Len * 3) / 4);
  if (approxBytes < 1024) return `${approxBytes} B`;
  if (approxBytes < 1024 * 1024) return `${(approxBytes / 1024).toFixed(1)} KB`;
  return `${(approxBytes / 1024 / 1024).toFixed(2)} MB`;
}

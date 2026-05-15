import type { FileTransfer } from "$lib/api/types";
import { formatFileSize } from "./format";

/** Integer 0-100 progress for a file transfer. Empty file → 100. */
export function transferProgress(t: FileTransfer): number {
  if (t.totalSize === 0) return 100;
  return Math.round((t.doneBytes / t.totalSize) * 100);
}

/** Instantaneous throughput string ("123 KB/s"). Empty when freshly started. */
export function transferSpeed(t: FileTransfer): string {
  const elapsed = (Date.now() - t.startedAt) / 1000;
  if (elapsed < 0.1) return "";
  const bps = t.doneBytes / elapsed;
  return `${formatFileSize(Math.round(bps))}/s`;
}

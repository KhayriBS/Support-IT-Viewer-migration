import { technicianApi } from "$lib/api";
import type { ControlSession } from "$lib/api";
import type { FileEntry, FileTransfer, FileTransferStartRequest, FileTransferUpdateRequest } from "$lib/api/types";

const FILE_CHANNEL_UPLOAD_BACKPRESSURE = 4 * 1024 * 1024; // 4 MB
const FILE_CHUNK_SIZE = 16 * 1024;                         // 16 KB

class FileChannel {
  channel: RTCDataChannel | null = null;
  fileChannelOpen = $state(false);
  fileListLoading = $state(false);
  fileListError = $state<string | null>(null);
  fileCurrentPath = $state("");
  fileListing = $state<FileEntry[]>([]);
  fileTransfers = $state<Record<string, FileTransfer>>({});
  /** Mirror of channel.readyState polled every 500 ms (Svelte cannot observe it directly). */
  rdFileChannelLive = $state(false);

  /** transferId of the download currently receiving binary chunks */
  activeDownloadId: string | null = null;

  // ── Callbacks set by the orchestrator ────────────────────────────────────
  getSession: () => ControlSession | null = () => null;
  getChatLocalRole: () => "viewer" | "agent" = () => "viewer";
  getLocalMachineId: () => string = () => "";

  // ── DataChannel wiring ───────────────────────────────────────────────────
  configure = (channel: RTCDataChannel) => {
    channel.binaryType = "arraybuffer";
    this.channel = channel;
    this.fileChannelOpen = channel.readyState === "open";
    console.log(`[file-ch] configure (readyState=${channel.readyState})`);

    channel.onopen = () => {
      this.fileChannelOpen = true;
      console.log(`[file-ch] OPENED (readyState=${channel.readyState})`);
    };
    channel.onclose = () => {
      console.warn(`[file-ch] CLOSED (was channel? ${this.channel === channel}, readyState=${channel.readyState})`);
      if (this.channel === channel) {
        this.fileChannelOpen = false;
        this.activeDownloadId = null;
      }
    };
    channel.onerror = (e) => {
      console.warn(`[file-ch] ERROR`, e);
      if (this.channel === channel) {
        this.fileChannelOpen = false;
      }
    };
    channel.onmessage = (event: MessageEvent<string | ArrayBuffer>) => {
      if (typeof event.data === "string") {
        try {
          this.handleJson(JSON.parse(event.data) as Record<string, unknown>);
        } catch {
          // ignore malformed JSON
        }
      } else if (event.data instanceof ArrayBuffer) {
        this.handleBinary(event.data);
      }
    };
  };

  handleJson = (msg: Record<string, unknown>) => {
    const type = msg.type as string | undefined;
    const tid = (msg.transferId as string | undefined) ?? "";

    if (type === "FILE_LIST_RESPONSE") {
      this.fileCurrentPath = (msg.path as string) ?? "";
      this.fileListing = (msg.files as FileEntry[]) ?? [];
      this.fileListError = (msg.error as string | null) ?? null;
      this.fileListLoading = false;
      return;
    }

    if (type === "FILE_DOWNLOAD_RESPONSE") {
      this.activeDownloadId = tid;
      const fileName = (msg.fileName as string) ?? "file";
      const totalSize = (msg.totalSize as number) ?? 0;
      this.fileTransfers = {
        ...this.fileTransfers,
        [tid]: {
          transferId: tid,
          type: "download",
          fileName,
          totalSize,
          totalChunks: (msg.totalChunks as number) ?? 1,
          doneChunks: 0,
          doneBytes: 0,
          startedAt: Date.now(),
          state: "active",
          buffers: []
        } satisfies FileTransfer
      };
      this.logTransferStartSafe({
        transferId: tid,
        sessionId: this.getSession()?.id ?? null,
        fromMachineId: this.peerMachineIdForLog(),
        toMachineId: this.getLocalMachineId(),
        direction: this.downloadDirectionForLog(),
        fileName,
        fileSize: totalSize,
        mimeType: null
      });
      return;
    }

    if (type === "FILE_COMPLETE") {
      const transfer = this.fileTransfers[tid];
      if (transfer?.type === "download" && transfer.state === "active") {
        const blob = new Blob(transfer.buffers ?? []);
        const url = URL.createObjectURL(blob);
        const a = document.createElement("a");
        a.href = url;
        a.download = transfer.fileName;
        document.body.appendChild(a);
        a.click();
        document.body.removeChild(a);
        setTimeout(() => URL.revokeObjectURL(url), 60_000);

        this.fileTransfers = {
          ...this.fileTransfers,
          [tid]: { ...transfer, state: "complete", buffers: undefined }
        };
        if (this.activeDownloadId === tid) {
          this.activeDownloadId = null;
        }
        this.logTransferUpdateSafe(tid, {
          status: "COMPLETED",
          fileSize: transfer.totalSize
        });
      } else if (transfer?.type === "upload") {
        this.fileTransfers = {
          ...this.fileTransfers,
          [tid]: { ...transfer, state: "complete" }
        };
        this.logTransferUpdateSafe(tid, {
          status: "COMPLETED",
          fileSize: transfer.totalSize
        });
      }
      return;
    }

    if (type === "FILE_UPLOAD_STARTED") {
      const destPath = (msg.destPath as string) ?? "";
      console.log(`[file-ch] agent confirms upload start tid=${tid} -> ${destPath}`);
      const transfer = this.fileTransfers[tid];
      if (transfer) {
        this.fileTransfers = {
          ...this.fileTransfers,
          [tid]: { ...transfer, error: undefined, ...(destPath ? { destPath } as Partial<FileTransfer> : {}) }
        };
      }
      return;
    }

    if (type === "FILE_UPLOAD_ACK") {
      const destPath = (msg.destPath as string) ?? "";
      const canonicalPath = (msg.canonicalPath as string) ?? "";
      const size = (msg.size as number) ?? 0;
      const finalPath = canonicalPath || destPath;
      console.log(`[file-ch] agent ACK tid=${tid} size=${size} -> ${finalPath}`);
      const transfer = this.fileTransfers[tid];
      if (transfer) {
        this.fileTransfers = {
          ...this.fileTransfers,
          [tid]: { ...transfer, state: "complete", ...(finalPath ? { destPath: finalPath } as Partial<FileTransfer> : {}) }
        };
        this.logTransferUpdateSafe(tid, {
          status: "COMPLETED",
          fileSize: size > 0 ? size : transfer.totalSize,
          destPath: finalPath || null
        });
      }
      return;
    }

    if (type === "FILE_ERROR") {
      const errMsg = (msg.message as string) ?? "unknown error";
      const transfer = this.fileTransfers[tid];
      if (transfer) {
        this.fileTransfers = {
          ...this.fileTransfers,
          [tid]: { ...transfer, state: "error", error: errMsg }
        };
      } else {
        console.error("[file-ch] remote error:", errMsg);
      }
      if (this.activeDownloadId === tid) {
        this.activeDownloadId = null;
      }
      if (tid) {
        this.logTransferUpdateSafe(tid, { status: "FAILED", errorMessage: errMsg });
      }
    }
  };

  handleBinary = (data: ArrayBuffer) => {
    const tid = this.activeDownloadId;
    if (!tid) return;
    const transfer = this.fileTransfers[tid];
    if (!transfer || transfer.type !== "download" || transfer.state !== "active") return;

    const updated: FileTransfer = {
      ...transfer,
      doneChunks: transfer.doneChunks + 1,
      doneBytes: transfer.doneBytes + data.byteLength,
      buffers: [...(transfer.buffers ?? []), data]
    };
    this.fileTransfers = { ...this.fileTransfers, [tid]: updated };
  };

  // ── Browse / download / upload ───────────────────────────────────────────
  requestFileList = (path: string) => {
    if (!this.channel || this.channel.readyState !== "open") {
      this.fileListError = "Canal fichier non disponible.";
      return;
    }
    this.fileListLoading = true;
    this.fileListError = null;
    this.channel.send(JSON.stringify({ type: "FILE_LIST_REQUEST", path }));
  };

  downloadRemoteFile = (filePath: string, fileName: string) => {
    if (!this.channel || this.channel.readyState !== "open") return;
    const tid = crypto.randomUUID();
    this.channel.send(JSON.stringify({ type: "FILE_DOWNLOAD_REQUEST", transferId: tid, path: filePath }));
    console.info("[file-ch] download requested:", fileName, tid);
  };

  uploadLocalFile = async (file: File) => {
    if (!this.channel || this.channel.readyState !== "open") return;

    const tid = crypto.randomUUID();
    const totalChunks = Math.max(1, Math.ceil(file.size / FILE_CHUNK_SIZE));

    const transfer: FileTransfer = {
      transferId: tid,
      type: "upload",
      fileName: file.name,
      totalSize: file.size,
      totalChunks,
      doneChunks: 0,
      doneBytes: 0,
      startedAt: Date.now(),
      state: "active"
    };
    this.fileTransfers = { ...this.fileTransfers, [tid]: transfer };

    this.channel.send(JSON.stringify({
      type: "FILE_UPLOAD_START",
      transferId: tid,
      fileName: file.name,
      totalSize: file.size,
      totalChunks
    }));

    this.logTransferStartSafe({
      transferId: tid,
      sessionId: this.getSession()?.id ?? null,
      fromMachineId: this.getLocalMachineId(),
      toMachineId: this.peerMachineIdForLog(),
      direction: this.uploadDirectionForLog(),
      fileName: file.name,
      fileSize: file.size,
      mimeType: file.type || null
    });

    for (let i = 0; i < totalChunks; i++) {
      if (!this.channel || this.channel.readyState !== "open") {
        this.fileTransfers = {
          ...this.fileTransfers,
          [tid]: { ...this.fileTransfers[tid], state: "error", error: "Canal ferme pendant l'envoi" }
        };
        this.logTransferUpdateSafe(tid, {
          status: "FAILED",
          errorMessage: "Canal ferme pendant l'envoi"
        });
        return;
      }
      while (this.channel.bufferedAmount > FILE_CHANNEL_UPLOAD_BACKPRESSURE) {
        await new Promise<void>((resolve) => setTimeout(resolve, 50));
      }

      const start = i * FILE_CHUNK_SIZE;
      const chunk = await file.slice(start, start + FILE_CHUNK_SIZE).arrayBuffer();
      try {
        this.channel.send(chunk);
      } catch (sendErr) {
        console.error(`[file-ch] send chunk #${i + 1} failed:`, sendErr);
        this.fileTransfers = {
          ...this.fileTransfers,
          [tid]: { ...this.fileTransfers[tid], state: "error", error: `send: ${String(sendErr)}` }
        };
        this.logTransferUpdateSafe(tid, {
          status: "FAILED",
          errorMessage: `send chunk #${i + 1}: ${String(sendErr)}`
        });
        return;
      }

      const prev = this.fileTransfers[tid];
      this.fileTransfers = {
        ...this.fileTransfers,
        [tid]: {
          ...prev,
          doneChunks: i + 1,
          doneBytes: Math.min(prev.doneBytes + chunk.byteLength, file.size)
        }
      };
    }

    this.channel.send(JSON.stringify({ type: "FILE_COMPLETE", transferId: tid }));

    setTimeout(() => {
      const cur = this.fileTransfers[tid];
      if (cur && cur.state === "active" && cur.doneChunks >= totalChunks) {
        this.fileTransfers = {
          ...this.fileTransfers,
          [tid]: { ...cur, state: "complete", doneBytes: file.size }
        };
        console.log(`[file-ch] upload tid=${tid} forced to complete (ACK timeout)`);
        this.logTransferUpdateSafe(tid, {
          status: "COMPLETED",
          fileSize: file.size
        });
      }
    }, 1000);
  };

  reset = () => {
    try { this.channel?.close(); } catch { /* ignore */ }
    this.channel = null;
    this.fileChannelOpen = false;
    this.fileListLoading = false;
    this.fileListError = null;
    this.fileListing = [];
    this.fileTransfers = {};
    this.activeDownloadId = null;
  };

  // ── UI helpers ───────────────────────────────────────────────────────────
  hasActiveTransfer = (): boolean => {
    for (const t of Object.values(this.fileTransfers)) {
      if (t.state === "active") return true;
    }
    return false;
  };

  dismissTransfer = (tid: string) => {
    const next = { ...this.fileTransfers };
    delete next[tid];
    this.fileTransfers = next;
  };

  progressPercent = (t: FileTransfer): number => {
    if (t.totalSize <= 0) return 0;
    return Math.min(100, Math.round((t.doneBytes / t.totalSize) * 100));
  };

  // ── Audit logging helpers ────────────────────────────────────────────────
  peerMachineIdForLog = (): string => {
    const session = this.getSession();
    if (!session) return "";
    if (this.getChatLocalRole() === "agent") {
      return session.technicianUsername ?? "";
    }
    return session.agentMachineId ?? "";
  };

  uploadDirectionForLog = (): "UPLOAD" | "DOWNLOAD" => {
    return this.getChatLocalRole() === "agent" ? "DOWNLOAD" : "UPLOAD";
  };

  downloadDirectionForLog = (): "UPLOAD" | "DOWNLOAD" => {
    return this.getChatLocalRole() === "agent" ? "UPLOAD" : "DOWNLOAD";
  };

  logTransferStartSafe = (payload: FileTransferStartRequest) => {
    void technicianApi.logFileTransferStart(payload).catch((err) => {
      console.warn("[file-log] start failed:", err);
    });
  };

  logTransferUpdateSafe = (transferId: string, payload: FileTransferUpdateRequest) => {
    void technicianApi.logFileTransferUpdate(transferId, payload).catch((err) => {
      console.warn("[file-log] update failed:", err);
    });
  };
}

export const fileChannel = new FileChannel();

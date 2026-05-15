<script lang="ts">
  import type { ControlSession } from "$lib/api";

  interface Props {
    open: boolean;
    session: ControlSession | null;
    errorMessage: string | null;
    loading: boolean;
    allowRemoteInput: boolean;
    allowFileTransfer: boolean;
    onApprove: () => void;
    onReject: () => void;
    onClose: () => void;
  }

  let {
    open,
    session,
    errorMessage,
    loading,
    allowRemoteInput = $bindable(),
    allowFileTransfer = $bindable(),
    onApprove,
    onReject,
    onClose
  }: Props = $props();
</script>

{#if open && session}
  <div
    class="rd-approval-overlay"
    role="dialog"
    tabindex="-1"
    onkeydown={(e) => { if (e.key === "Escape" && !loading) onClose(); }}
    onmousedown={(e) => { if (!loading && e.target === e.currentTarget) onClose(); }}>
    <div class="rd-approval-modal">
      <h2>Demande d'accès distant</h2>
      <p class="rd-approval-desc">
        <strong>{session.technicianUsername || "Un technicien"}</strong>
        demande l'accès à ce PC.
      </p>

      {#if errorMessage}
        <p class="rd-approval-error">{errorMessage}</p>
      {/if}

      <div class="rd-approval-options">
        <label>
          <input type="checkbox" bind:checked={allowRemoteInput} disabled={loading} />
          Autoriser clavier / souris
        </label>
        <label>
          <input type="checkbox" bind:checked={allowFileTransfer} disabled={loading} />
          Autoriser transfert de fichiers
        </label>
      </div>

      <div class="rd-approval-actions">
        <button
          class="rd-approval-btn rd-approval-btn--reject"
          onclick={onReject}
          disabled={loading}>
          {loading ? "Traitement…" : "Refuser"}
        </button>
        <button
          class="rd-approval-btn rd-approval-btn--approve"
          onclick={onApprove}
          disabled={loading}>
          {loading ? "Traitement…" : "Autoriser"}
        </button>
      </div>
    </div>
  </div>
{/if}

<style>
  .rd-approval-overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.65);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
    backdrop-filter: blur(2px);
  }
  .rd-approval-modal {
    background: #11181f;
    border: 1px solid #1f2a36;
    border-radius: 14px;
    padding: 26px 28px;
    width: min(92vw, 460px);
    box-shadow: 0 20px 60px rgba(0, 0, 0, 0.55);
  }
  .rd-approval-modal h2 {
    margin: 0 0 8px 0;
    font-size: 18px;
    color: #fff;
  }
  .rd-approval-desc {
    margin: 0 0 18px 0;
    font-size: 14px;
    color: #cbd5e1;
  }
  .rd-approval-desc strong { color: #38bdf8; }
  .rd-approval-error {
    margin: 0 0 14px 0;
    padding: 8px 12px;
    border-radius: 8px;
    background: rgba(239, 68, 68, 0.12);
    border: 1px solid rgba(239, 68, 68, 0.3);
    color: #fca5a5;
    font-size: 13px;
  }
  .rd-approval-options {
    display: flex;
    flex-direction: column;
    gap: 10px;
    margin-bottom: 22px;
  }
  .rd-approval-options label {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 10px 12px;
    background: #0a0f15;
    border: 1px solid #1f2a36;
    border-radius: 8px;
    cursor: pointer;
    font-size: 14px;
    color: #e2e8f0;
  }
  .rd-approval-options label:hover { background: #0f1620; }
  .rd-approval-options input[type="checkbox"] {
    width: 16px;
    height: 16px;
    accent-color: #38bdf8;
  }
  .rd-approval-actions {
    display: flex;
    justify-content: flex-end;
    gap: 10px;
  }
  .rd-approval-btn {
    padding: 10px 20px;
    border-radius: 8px;
    font-size: 14px;
    font-weight: 500;
    cursor: pointer;
    border: 1px solid transparent;
    transition: background 0.15s;
  }
  .rd-approval-btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .rd-approval-btn--reject {
    background: transparent;
    border-color: #1f2a36;
    color: #cbd5e1;
  }
  .rd-approval-btn--reject:hover:not(:disabled) {
    background: rgba(239, 68, 68, 0.1);
    border-color: rgba(239, 68, 68, 0.4);
    color: #fca5a5;
  }
  .rd-approval-btn--approve {
    background: #38bdf8;
    color: #0d1117;
  }
  .rd-approval-btn--approve:hover:not(:disabled) { background: #7dd3fc; }
</style>

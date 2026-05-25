<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onDestroy, onMount } from "svelte";

  // ── Local reactive state ──────────────────────────────────────────────
  let autostartEnabled = $state<boolean>(false);
  let autostartBusy = $state<boolean>(false);
  let autostartError = $state<string | null>(null);

  let statusLabel = $state<string>("Connexion au serveur…");
  let machineId = $state<string>("");
  // 'online' = authenticated, idle | 'session' = in a remote session
  // | 'offline' = not running yet / authentication failed
  let statusKind = $state<"online" | "session" | "offline">("offline");

  let pollTimer: ReturnType<typeof setInterval> | null = null;

  // The Rust `AgentStatus` payload (camelCase via serde rename_all).
  interface RustAgentStatus {
    running: boolean;
    authenticated: boolean;
    inSession: boolean;
    machineId: string;
    serverUrl: string;
    sessionId: number | null;
    technician: string | null;
  }

  function classifyStatus(s: RustAgentStatus): "online" | "session" | "offline" {
    if (s.inSession) return "session";
    if (s.authenticated) return "online";
    return "offline";
  }

  async function refreshStatus(): Promise<void> {
    try {
      const [label, raw] = await Promise.all([
        invoke<string>("get_agent_status_label"),
        invoke<RustAgentStatus>("get_agent_status")
      ]);
      statusLabel = label;
      statusKind = classifyStatus(raw);
      machineId = raw.machineId ?? "";
    } catch (err) {
      // Backend not ready yet on first paint — silent retry on the
      // poll timer; surfacing this in the UI would be noisy.
      console.debug("[AgentStatus] refreshStatus failed:", err);
    }
  }

  async function refreshAutostart(): Promise<void> {
    try {
      autostartEnabled = await invoke<boolean>("get_autostart_status");
    } catch (err) {
      console.warn("[AgentStatus] get_autostart_status failed:", err);
    }
  }

  async function toggleAutostart(event: Event): Promise<void> {
    const target = event.currentTarget as HTMLInputElement;
    const next = target.checked;
    autostartBusy = true;
    autostartError = null;
    try {
      await invoke("set_autostart", { enabled: next });
      autostartEnabled = next;
    } catch (err) {
      autostartError = String(err);
      // Re-sync the checkbox with the actual registry state on failure.
      target.checked = autostartEnabled;
    } finally {
      autostartBusy = false;
    }
  }

  onMount(() => {
    void refreshStatus();
    void refreshAutostart();
    // 2 s is a good balance — status changes are driven by user
    // actions, not high-frequency events; polling tighter wastes IPC.
    pollTimer = setInterval(() => {
      void refreshStatus();
    }, 2000);
  });

  onDestroy(() => {
    if (pollTimer) clearInterval(pollTimer);
  });
</script>

<section class="agent-status">
  <div class="agent-status__card">
    <div class="agent-status__indicator-row">
      <span
        class="agent-status__dot"
        class:agent-status__dot--online={statusKind === "online"}
        class:agent-status__dot--session={statusKind === "session"}
        class:agent-status__dot--offline={statusKind === "offline"}
        aria-hidden="true"
      ></span>
      <div class="agent-status__textcol">
        <span class="agent-status__label">{statusLabel}</span>
        {#if machineId}
          <span class="agent-status__machine" title="Identifiant de cette machine">
            {machineId}
          </span>
        {/if}
      </div>
    </div>
  </div>

  <div class="agent-status__autostart">
    <label class="agent-status__switch-row">
      <span class="agent-status__switch-text">
        <strong>Demarrer avec Windows</strong>
        <small>L'agent demarre automatiquement avec Windows</small>
        {#if !autostartEnabled}
          <small class="agent-status__warn">
            L'agent ne sera pas disponible apres redemarrage.
          </small>
        {/if}
      </span>
      <span class="agent-status__switch">
        <input
          type="checkbox"
          bind:checked={autostartEnabled}
          onchange={toggleAutostart}
          disabled={autostartBusy}
          aria-label="Activer le demarrage automatique"
        />
        <span class="agent-status__switch-track" aria-hidden="true"></span>
      </span>
    </label>

    {#if autostartError}
      <p class="agent-status__error" role="alert">
        Echec : {autostartError}
      </p>
    {/if}

    <span
      class="agent-status__badge"
      class:agent-status__badge--on={autostartEnabled}
      class:agent-status__badge--off={!autostartEnabled}
    >
      {autostartEnabled ? "Demarrage auto actif" : "Manuel uniquement"}
    </span>
  </div>
</section>

<style>
  .agent-status {
    display: flex;
    flex-direction: column;
    gap: 0.85rem;
    padding: 1rem;
    border-radius: 12px;
    background: var(--rd-surface, #ffffff);
    border: 1px solid var(--rd-border, #e2e8f0);
  }

  .agent-status__card {
    display: flex;
    align-items: center;
  }

  .agent-status__indicator-row {
    display: flex;
    align-items: center;
    gap: 0.7rem;
    width: 100%;
  }

  .agent-status__dot {
    width: 12px;
    height: 12px;
    border-radius: 50%;
    flex-shrink: 0;
    background: #94a3b8;
    box-shadow: 0 0 0 0 rgba(0, 0, 0, 0);
  }
  .agent-status__dot--online {
    background: #22c55e;
    animation: agent-status-pulse 1.6s ease-out infinite;
  }
  .agent-status__dot--session {
    background: #f97316;
    animation: agent-status-pulse 1.1s ease-out infinite;
  }
  .agent-status__dot--offline {
    background: #94a3b8;
  }

  @keyframes agent-status-pulse {
    0%   { box-shadow: 0 0 0 0 rgba(34, 197, 94, 0.55); }
    70%  { box-shadow: 0 0 0 9px rgba(34, 197, 94, 0); }
    100% { box-shadow: 0 0 0 0 rgba(34, 197, 94, 0); }
  }

  .agent-status__textcol {
    display: flex;
    flex-direction: column;
  }
  .agent-status__label {
    font-weight: 600;
    font-size: 0.95rem;
    color: var(--rd-text, #0f172a);
  }
  .agent-status__machine {
    font-size: 0.78rem;
    color: var(--rd-text-muted, #64748b);
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  }

  .agent-status__autostart {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    padding-top: 0.6rem;
    border-top: 1px solid var(--rd-border, #e2e8f0);
  }

  .agent-status__switch-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
    cursor: pointer;
    user-select: none;
  }

  .agent-status__switch-text {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
  }
  .agent-status__switch-text strong {
    font-size: 0.9rem;
    color: var(--rd-text, #0f172a);
  }
  .agent-status__switch-text small {
    font-size: 0.78rem;
    color: var(--rd-text-muted, #64748b);
  }
  .agent-status__warn {
    color: #b35900;
  }

  /* CSS-only toggle — no framework. */
  .agent-status__switch {
    position: relative;
    width: 42px;
    height: 24px;
    flex-shrink: 0;
  }
  .agent-status__switch input[type="checkbox"] {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    opacity: 0;
    cursor: pointer;
    margin: 0;
  }
  .agent-status__switch-track {
    position: absolute;
    inset: 0;
    background: #cbd5e1;
    border-radius: 999px;
    transition: background 150ms ease;
  }
  .agent-status__switch-track::after {
    content: "";
    position: absolute;
    top: 2px;
    left: 2px;
    width: 20px;
    height: 20px;
    background: #ffffff;
    border-radius: 50%;
    transition: transform 150ms ease;
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.25);
  }
  .agent-status__switch input:checked + .agent-status__switch-track {
    background: #22c55e;
  }
  .agent-status__switch input:checked + .agent-status__switch-track::after {
    transform: translateX(18px);
  }
  .agent-status__switch input:disabled + .agent-status__switch-track {
    opacity: 0.55;
  }

  .agent-status__badge {
    align-self: flex-start;
    padding: 0.2rem 0.6rem;
    border-radius: 999px;
    font-size: 0.74rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.02em;
  }
  .agent-status__badge--on {
    background: #22c55e;
    color: #ffffff;
  }
  .agent-status__badge--off {
    background: #ef6c00;
    color: #ffffff;
  }

  .agent-status__error {
    margin: 0;
    font-size: 0.82rem;
    color: #b91c1c;
  }
</style>

<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { technicianApi } from "$lib/api";
  import { agentManager } from "$lib/managers/agent-manager.svelte";

  // Ping réel toutes les 5 s avec timeout 2 s. navigator.onLine est ignoré
  // (peu fiable sur webview Windows : il rapporte true même WiFi coupé).
  let online = $state(true);
  let pingTimer: ReturnType<typeof setInterval> | null = null;

  async function pingServer() {
    const ctl = new AbortController();
    const timeoutId = setTimeout(() => ctl.abort(), 2000);
    try {
      const res = await fetch(`${technicianApi.baseUrl}/auth/machine-status/__ping__`, {
        method: "GET",
        signal: ctl.signal,
        cache: "no-store"
      });
      online = res.ok || res.status === 404; // 404 reste une preuve que le serveur a répondu
    } catch {
      online = false;
    } finally {
      clearTimeout(timeoutId);
    }
  }

  onMount(() => {
    void pingServer();
    pingTimer = setInterval(pingServer, 5000);
  });

  onDestroy(() => {
    if (pingTimer) clearInterval(pingTimer);
  });
</script>

<section class="rd-panel">
  <h2 class="rd-panel__title"><span class="rd-icon">🖥</span> Métriques de cette machine</h2>
  <div class="rd-metrics">
    <div class="rd-metric">
      <div class="rd-metric__head"><span class="rd-metric__icon rd-metric__icon--cpu">⚙</span> CPU</div>
      <div class="rd-metric__value">{agentManager.metrics ? `${agentManager.metrics.cpuUsage.toFixed(0)}%` : "24%"}</div>
    </div>
    <div class="rd-metric">
      <div class="rd-metric__head"><span class="rd-metric__icon rd-metric__icon--ram">🗄</span> RAM</div>
      <div class="rd-metric__value">{agentManager.metrics ? `${(agentManager.metrics.ramUsage / 100 * 16).toFixed(1)} / 16 GB` : "8.2 / 16 GB"}</div>
    </div>
    <div class="rd-metric">
      <div class="rd-metric__head"><span class="rd-metric__icon rd-metric__icon--disk">💾</span> Disque</div>
      <div class="rd-metric__value">{agentManager.metrics ? `${(agentManager.metrics.diskUsage / 100 * 512).toFixed(0)} / 512 GB` : "256 / 512 GB"}</div>
    </div>
    <div class="rd-metric">
      <div class="rd-metric__head"><span class="rd-metric__icon rd-metric__icon--net">📶</span> Réseau</div>
      <div class="rd-metric__value {online ? 'rd-metric__value--ok' : 'rd-metric__value--ko'}">
        {online ? "Connecté" : "Non connecté"}
      </div>
    </div>
  </div>
</section>

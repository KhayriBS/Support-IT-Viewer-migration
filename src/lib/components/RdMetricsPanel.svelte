<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { agentManager } from "$lib/managers/agent-manager.svelte";

  // Statut internet réel via navigator.onLine + events online/offline.
  let online = $state(
    typeof navigator !== "undefined" ? navigator.onLine : true
  );

  function syncOnline() { online = navigator.onLine; }

  onMount(() => {
    window.addEventListener("online", syncOnline);
    window.addEventListener("offline", syncOnline);
  });

  onDestroy(() => {
    window.removeEventListener("online", syncOnline);
    window.removeEventListener("offline", syncOnline);
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

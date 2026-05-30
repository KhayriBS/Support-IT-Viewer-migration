<script lang="ts">
  import "../app.css";
  import { onDestroy, onMount } from "svelte";
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import RdApprovalModal from "$lib/components/RdApprovalModal.svelte";
  import { agentManager } from "$lib/managers/agent-manager.svelte";
  import { approvalManager, type ApprovalDecision } from "$lib/managers/approval-manager.svelte";
  import { sessionManager } from "$lib/managers/session-manager.svelte";

  let { children } = $props();

  let approvalTimer: ReturnType<typeof setInterval> | null = null;
  let pendingPollTimer: ReturnType<typeof setInterval> | null = null;
  let realUnloadInProgress = false;

  approvalManager.onApproved = (decision: ApprovalDecision) => {
    sessionManager.activeSession = {
      ...decision.session,
      allowRemoteInput: decision.allowRemoteInput,
      allowFileTransfer: decision.allowFileTransfer,
      status: "ACTIVE"
    };
    sessionManager.queriedSession = sessionManager.activeSession;
    sessionManager.selectedFeature = null;
    sessionManager.waitingForApproval = false;
  };

  function routeForRole(role: string): string {
    if (role === "TECHNICIAN") return "/dashboard";
    if (role === "USER") return "/my-machines";
    return "/pending";
  }

  $effect(() => {
    const role = agentManager.role;
    if (!role) return;
    const target = routeForRole(role);
    if (page.url.pathname === "/" || page.url.pathname === target) {
      if (page.url.pathname !== target) {
        void goto(target, { replaceState: true });
      }
      return;
    }
    // Si on est sur une page incompatible avec le rôle, on redirige.
    const allowedFor: Record<string, string[]> = {
      TECHNICIAN: ["/dashboard"],
      USER: ["/my-machines"],
      PENDING: ["/pending"]
    };
    const allowed = allowedFor[role] ?? [];
    if (!allowed.some((p) => page.url.pathname.startsWith(p))) {
      void goto(target, { replaceState: true });
    }
  });

  onMount(() => {
    void agentManager.syncLifecycle();
    void agentManager.loadMachineId();
    void approvalManager.check();
    approvalTimer = setInterval(approvalManager.check, 3000);

    // Polling PENDING : refresh role toutes les 30 s tant qu'on n'a pas
    // d'assignation. S'arrête naturellement dès que role devient USER/TECHNICIAN.
    pendingPollTimer = setInterval(() => {
      if (agentManager.role === "PENDING" || agentManager.role === "") {
        void agentManager.refreshRole();
      }
    }, 30_000);

    if (typeof window !== "undefined") {
      const markRealUnload = () => { realUnloadInProgress = true; };
      window.addEventListener("beforeunload", markRealUnload);
      window.addEventListener("pagehide", markRealUnload);
    }
  });

  onDestroy(() => {
    if (approvalTimer) clearInterval(approvalTimer);
    if (pendingPollTimer) clearInterval(pendingPollTimer);
    if (!realUnloadInProgress) return;
    void agentManager.stopLifecycle();
  });
</script>

{@render children?.()}

<!-- Approval modal : pop-up côté ordinateur DISTANT (cible) sur TOUTES les routes -->
<RdApprovalModal
  open={approvalManager.open}
  session={approvalManager.pendingSession}
  errorMessage={approvalManager.error}
  loading={approvalManager.loading}
  bind:allowRemoteInput={approvalManager.allowRemoteInput}
  bind:allowFileTransfer={approvalManager.allowFileTransfer}
  onApprove={approvalManager.approve}
  onReject={approvalManager.reject}
  onClose={approvalManager.close} />

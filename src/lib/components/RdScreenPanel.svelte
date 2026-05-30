<script lang="ts">
  import type { ChatMessage, ControlSession, TypingNotification } from "$lib/api";
  import { signalBus } from "$lib/managers/signal-bus.svelte";
  import { viewerPeer } from "$lib/managers/viewer-peer.svelte";
  import { fileChannel } from "$lib/managers/file-channel.svelte";
  import { aiPipeline } from "$lib/managers/ai-pipeline.svelte";
  import RdViewerStatsBar from "./RdViewerStatsBar.svelte";
  import RdQualitySelector from "./RdQualitySelector.svelte";
  import RdChatList from "./RdChatList.svelte";
  import RdChatCompose from "./RdChatCompose.svelte";
  import RdTransferList from "./RdTransferList.svelte";

  interface Props {
    session: ControlSession;
    actionLoading: boolean;
    chatConnected: boolean;
    chatLocalRole: string;
    messages: ChatMessage[];
    typing: TypingNotification | null;
    chatListEl: HTMLDivElement | null;
    chatInput: string;
    rdScreenPlayRequested: boolean;
    rdVideoPausedForTransfer: boolean;
    fileInputEl: HTMLInputElement | null;
    onBackToMenu: () => void;
    onPlay: () => void;
    onPause: () => void;
    onConnectChat: () => void;
    onTriggerFilePicker: () => void;
    onFilePicked: (e: Event) => void;
    onSendMessage: () => void;
    onDispatchTyping: () => void;
    onDisconnect: () => void;
  }

  let {
    session,
    actionLoading,
    chatConnected,
    chatLocalRole,
    messages,
    typing,
    chatListEl = $bindable(),
    chatInput = $bindable(),
    rdScreenPlayRequested,
    rdVideoPausedForTransfer,
    fileInputEl = $bindable(),
    onBackToMenu,
    onPlay,
    onPause,
    onConnectChat,
    onTriggerFilePicker,
    onFilePicked,
    onSendMessage,
    onDispatchTyping,
    onDisconnect
  }: Props = $props();
</script>

<section class="rd-panel rd-viewer">
  <header class="rd-viewer__head">
    <h2 class="rd-panel__title">
      <span class="rd-icon">🖥</span>
      Session en cours avec
      <strong class="rd-viewer__peer">{session.agentMachineId}</strong>
    </h2>
    <p class="rd-viewer__sub">
      {#if signalBus.signalingConnected && viewerPeer.viewerRemoteStream}
        Stream actif
        {#if viewerPeer.viewerStreamMbps !== null}&nbsp;•&nbsp; {viewerPeer.viewerStreamMbps.toFixed(1)} Mbps{/if}
        {#if viewerPeer.viewerStreamFps !== null}&nbsp;•&nbsp; {viewerPeer.viewerStreamFps.toFixed(0)} fps{/if}
      {:else if signalBus.signalingConnected}
        Signalisation connectée — attente de la première image…
      {:else}
        Connexion en cours…
      {/if}
    </p>
  </header>

  <div
    bind:this={viewerPeer.viewerShellEl}
    class="rd-viewer__stage"
    class:rd-viewer__stage--ready={!!viewerPeer.viewerRemoteStream}
    class:rd-viewer__stage--fullscreen={viewerPeer.viewerFullscreenActive}
    onmousemove={viewerPeer.revealViewerControls}
    role="presentation">
    {#if viewerPeer.viewerRemoteStream}
      <!-- svelte-ignore a11y_media_has_caption -->
      <video
        class="rd-viewer__video"
        class:active={viewerPeer.canSendViewerInput()}
        bind:this={viewerPeer.viewerVideoEl}
        autoplay
        playsinline
        muted
        tabindex="0"
        onfocus={viewerPeer.handleViewerVideoFocus}
        onblur={viewerPeer.handleViewerVideoBlur}
        onmousemove={viewerPeer.handleViewerMouseMove}
        onmousedown={viewerPeer.handleViewerMouseDown}
        onmouseup={viewerPeer.handleViewerMouseUp}
        onwheel={viewerPeer.handleViewerWheel}
        oncontextmenu={(event) => event.preventDefault()}
      ></video>
    {:else}
      <div class="rd-viewer__placeholder">
        <span class="rd-spinner"></span>
        <p>Réception de la première image WebRTC…</p>
      </div>
    {/if}

    {#if rdVideoPausedForTransfer}
      <div class="rd-viewer__transfer-overlay">
        <span class="rd-spinner"></span>
        <p>Transfert de fichier en cours — émission de frames suspendue côté agent.</p>
      </div>
    {:else if !rdScreenPlayRequested}
      <div class="rd-viewer__transfer-overlay">
        <button class="rd-viewer__big-play" type="button" onclick={onPlay}>
          <span class="rd-viewer__big-play-icon">▶</span>
          <span>Reprendre la diffusion</span>
        </button>
        <p>Émission suspendue. Clique pour la reprendre.</p>
      </div>
    {/if}

    <RdViewerStatsBar
      bind:visible={viewerPeer.viewerStatsBarVisible}
      streamPresent={!!viewerPeer.viewerRemoteStream}
      fps={viewerPeer.viewerLocalFps}
      mbps={viewerPeer.viewerLocalMbps}
      rttMs={viewerPeer.viewerLocalRttMs}
      lossPct={viewerPeer.viewerLocalLossPct}
      jitterMs={viewerPeer.viewerLocalJitterMs}
      resolution={viewerPeer.viewerLocalResolution} />

    <div
      class="rd-viewer__floating-actions"
      class:visible={viewerPeer.viewerControlsVisible || !viewerPeer.viewerRemoteStream || !rdScreenPlayRequested}>
      <button class="rd-viewer__fab" type="button" onclick={onBackToMenu} title="Retour au menu">
        <span class="rd-viewer__fab-icon">←</span>
        <span class="rd-viewer__fab-label">Menu</span>
      </button>
      <RdQualitySelector />
      {#if rdScreenPlayRequested}
        <button
          class="rd-viewer__fab"
          type="button"
          onclick={onPause}
          disabled={rdVideoPausedForTransfer}
          title="Suspendre l'émission des frames">
          <span class="rd-viewer__fab-icon">⏸</span>
          <span class="rd-viewer__fab-label">Pause</span>
        </button>
      {:else}
        <button
          class="rd-viewer__fab rd-viewer__fab--accent"
          type="button"
          onclick={onPlay}
          disabled={rdVideoPausedForTransfer}
          title="Démarrer l'émission de frames">
          <span class="rd-viewer__fab-icon">▶</span>
          <span class="rd-viewer__fab-label">Play</span>
        </button>
      {/if}
      <button
        class="rd-viewer__fab"
        type="button"
        onclick={onTriggerFilePicker}
        disabled={!fileChannel.rdFileChannelLive}
        title={fileChannel.rdFileChannelLive ? "Envoyer un fichier" : "Canal fichier non disponible"}>
        <span class="rd-viewer__fab-icon">📤</span>
        <span class="rd-viewer__fab-label">Fichier</span>
      </button>
      <button
        class="rd-viewer__fab"
        class:rd-viewer__fab--accent={viewerPeer.viewerChatPanelOpen}
        type="button"
        onclick={() => {
          viewerPeer.viewerChatPanelOpen = !viewerPeer.viewerChatPanelOpen;
          if (viewerPeer.viewerChatPanelOpen) onConnectChat();
        }}
        title={viewerPeer.viewerChatPanelOpen ? "Fermer le chat" : "Ouvrir le chat"}>
        <span class="rd-viewer__fab-icon">💬</span>
        <span class="rd-viewer__fab-label">Chat</span>
      </button>
      <button
        class="rd-viewer__fab"
        type="button"
        onclick={() => void viewerPeer.enterViewerFullscreen()}
        disabled={!viewerPeer.viewerRemoteStream || viewerPeer.viewerFullscreenActive}
        title="Plein écran">
        <span class="rd-viewer__fab-icon">⛶</span>
        <span class="rd-viewer__fab-label">Plein écran</span>
      </button>
      <button
        class="rd-viewer__fab"
        type="button"
        onclick={() => void viewerPeer.exitViewerFullscreen()}
        disabled={!viewerPeer.viewerFullscreenActive}
        title="Quitter le plein écran">
        <span class="rd-viewer__fab-icon">⤢</span>
        <span class="rd-viewer__fab-label">Quitter</span>
      </button>
      <button
        class="rd-viewer__fab rd-viewer__fab--danger"
        type="button"
        onclick={onDisconnect}
        disabled={actionLoading}
        title="Déconnecter la session">
        <span class="rd-viewer__fab-icon">⏻</span>
        <span class="rd-viewer__fab-label">Déconnecter</span>
      </button>
    </div>

    {#if viewerPeer.viewerChatPanelOpen}
      <aside class="rd-viewer__chat-side">
        <header class="rd-viewer__chat-side-head">
          <strong>💬 Chat</strong>
          <span class="rd-chat__pill" class:rd-chat__pill--ok={chatConnected} class:rd-chat__pill--warn={!chatConnected}>
            {chatConnected ? "Connecté" : "Hors ligne"}
          </span>
          <span class="rd-chat__pill" class:rd-chat__pill--ok={aiPipeline.aiConnected} class:rd-chat__pill--warn={!aiPipeline.aiConnected}>
            IA&nbsp;: {aiPipeline.aiConnected ? (aiPipeline.aiBusy ? "Analyse…" : "Prête") : "Hors ligne"}
          </span>
          <button
            class="rd-viewer__chat-side-close"
            type="button"
            onclick={() => { viewerPeer.viewerChatPanelOpen = false; }}
            title="Fermer">×</button>
        </header>

        {#if aiPipeline.aiError}
          <div class="rd-ai-error" role="alert">
            <span class="rd-ai-error__icon">⚠️</span>
            <span class="rd-ai-error__text">{aiPipeline.aiError}</span>
            <button class="rd-ai-error__close" type="button" onclick={() => { aiPipeline.aiError = null; }} title="Masquer">×</button>
          </div>
        {/if}

        {#if aiPipeline.aiLastVerificationImage}
          <div class="rd-ai-verif">
            <div class="rd-ai-verif__head">
              <span>📸 Screenshot de verification IA</span>
              <button class="rd-viewer__btn" type="button" onclick={() => { aiPipeline.aiLastVerificationImage = null; }}>Fermer</button>
            </div>
            <img src={aiPipeline.aiLastVerificationImage} alt="Screenshot de verification IA" class="rd-ai-verif__img" />
          </div>
        {/if}

        <RdChatList
          {messages}
          localRole={chatLocalRole}
          {typing}
          bind:listEl={chatListEl}
          emptyText="Aucun message. Envoie le premier !"
          typingText="écrit…"
          extraClass="rd-viewer__chat-side-list" />

        <RdChatCompose
          bind:value={chatInput}
          placeholder="Message au technicien… (ou tape ici puis 🤖 pour demander à l'IA)"
          onSend={onSendMessage}
          onSendAi={() => void aiPipeline.sendChatAsAi()}
          aiConnected={aiPipeline.aiConnected}
          aiBusy={aiPipeline.aiBusy}
          onInput={onDispatchTyping}
          extraClass="rd-viewer__chat-side-compose" />
      </aside>
    {/if}
  </div>

  <input
    bind:this={fileInputEl}
    type="file"
    multiple
    style="display:none"
    onchange={onFilePicked} />

  {#if Object.keys(fileChannel.fileTransfers).length > 0}
    <RdTransferList
      transfers={fileChannel.fileTransfers}
      variant="compact"
      showHeader
      channelOpen={fileChannel.fileChannelOpen} />
  {/if}

  {#if viewerPeer.screenFrameError}
    <p class="rd-connect__status rd-connect__status--error">{viewerPeer.screenFrameError}</p>
  {/if}
</section>

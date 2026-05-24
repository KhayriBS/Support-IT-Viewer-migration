# Analyse Détaillée du Projet **Lumière Tech-IT**

> Document d'analyse technique complète du projet de fin d'études (PFE)
> Plateforme intelligente de support informatique à distance

---

## Table des matières

1. [Identité du Projet](#1-identité-du-projet)
2. [Stack Technique](#2-stack-technique)
3. [Architecture Générale](#3-architecture-générale)
4. [Rôle Détaillé de Tauri 2.x](#4-rôle-détaillé-de-tauri-2x-rust--webview)
5. [Rôle Détaillé de SvelteKit / Svelte 5](#5-rôle-détaillé-de-sveltekit--svelte-5)
6. [Collaboration Tauri ↔ Frontend](#6-collaboration-tauri--frontend)
7. [Backend Rust — Modules de l'Agent](#7-backend-rust--modules-de-lagent)
8. [Frontend — Composants UI](#8-frontend--composants-ui)
9. [Frontend — Managers d'État](#9-frontend--managers-détat)
10. [Communication WebRTC et Signaling](#10-communication-webrtc-et-signaling)
11. [Capture d'Écran et Encodage Vidéo](#11-capture-décran-et-encodage-vidéo)
12. [Intégration de l'Intelligence Artificielle](#12-intégration-de-lintelligence-artificielle)
13. [Transfert de Fichiers](#13-transfert-de-fichiers)
14. [Chat Temps Réel](#14-chat-temps-réel)
15. [État Actuel du Développement](#15-état-actuel-du-développement)
16. [Synthèse des 5 Piliers Techniques](#16-synthèse-des-5-piliers-techniques)

---

## 1. Identité du Projet

**Lumière Tech-IT** est une **plateforme intelligente de support informatique à distance** développée dans le cadre d'un projet de fin d'études (PFE) sur 6 mois.

### Objectif
Permettre à un technicien IT de prendre le contrôle d'un poste utilisateur à distance, de manière sécurisée, peer-to-peer (P2P) et augmentée par l'intelligence artificielle.

### Comparaison
Application desktop unifiée comparable à **TeamViewer**, **AnyDesk** ou **Chrome Remote Desktop**, mais avec :
- Une architecture **propriétaire P2P** (pas de relais central pour la vidéo)
- Une **couche IA intégrée** capable d'analyser l'écran et d'exécuter des actions
- Un **backend Spring Boot** centralisé pour l'auth, la traçabilité et la gouvernance
- Un binaire **léger** (~10 Mo) et **multi-rôle** (un seul exécutable pour agent et viewer)

---

## 2. Stack Technique

| Couche | Technologie | Version | Rôle |
|--------|------------|---------|------|
| **Desktop** | Tauri | 2.x | Conteneur natif (fenêtre OS + pont JS↔Rust) |
| **Frontend UI** | SvelteKit + Svelte | 5 | Interface utilisateur réactive |
| **Langage front** | TypeScript | — | Typage statique |
| **Bundler** | Vite | — | Build des assets web |
| **Backend embarqué** | Rust | Edition 2021 | Logique système, WebRTC, encodage |
| **Serveur central** | Spring Boot | Java | Auth, signaling, orchestration |
| **Temps réel P2P** | WebRTC | — | Vidéo, données, fichiers, IA |
| **Signaling** | WebSocket | — | Échange SDP/ICE via Spring Boot |
| **Codec vidéo** | H.264 | — | OpenH264 / MediaFoundation / FFmpeg |
| **Capture écran** | DXGI / WGC | — | Windows Desktop Duplication / Graphics Capture |
| **Contrôle système** | enigo (Rust) | — | Mouse/keyboard programmatique |
| **IA** | Service Realtime | WebSocket | Analyse screenshot + actions guidées |

---

## 3. Architecture Générale

```
┌──────────────────────────────────────────────────────────────────┐
│                    SERVEUR CENTRAL (Spring Boot)                  │
│   - Authentification utilisateurs                                 │
│   - Signaling WebSocket (échange SDP/ICE)                         │
│   - Historique sessions & transferts                              │
│   - API REST (technicianApi)                                      │
└────────────────┬───────────────────────────┬─────────────────────┘
                 │ WebSocket + REST          │ WebSocket + REST
                 │                           │
        ┌────────▼────────┐         ┌────────▼────────┐
        │   AGENT (PC     │         │   VIEWER (PC    │
        │   utilisateur)  │ ◄─────► │   technicien)   │
        │                 │  P2P    │                 │
        │  Binaire Tauri  │ WebRTC  │  Binaire Tauri  │
        │  (même .exe)    │         │  (même .exe)    │
        └─────────────────┘         └─────────────────┘
                  ▲                           ▲
                  │                           │
                  └───────────┬───────────────┘
                              │
                       ┌──────▼──────┐
                       │ Service IA  │
                       │ (Realtime)  │
                       └─────────────┘
```

### Structure des dossiers

```
lumiere-tech-it/
├── src-tauri/                  ← Backend Rust (Tauri)
│   ├── src/
│   │   ├── main.rs             ← Point d'entrée minimal
│   │   ├── lib.rs              ← AppState + commandes Tauri
│   │   └── agent/              ← 18 modules métier
│   │       ├── mod.rs
│   │       ├── webrtc.rs
│   │       ├── adaptive_streaming.rs
│   │       ├── stream_senders.rs
│   │       ├── screen_capture.rs
│   │       ├── file_channel_handler.rs
│   │       ├── ai_executor.rs
│   │       ├── input_handler.rs
│   │       ├── signaling.rs
│   │       ├── session.rs
│   │       ├── metrics.rs
│   │       ├── auth.rs
│   │       ├── ice_servers.rs
│   │       ├── h264_helpers.rs
│   │       ├── video_encoder.rs
│   │       ├── media_foundation_encoder.rs
│   │       ├── desktop_duplication.rs
│   │       ├── file_transfer.rs
│   │       └── capture/        ← Sous-module : backends DXGI/WGC
│   ├── Cargo.toml
│   └── tauri.conf.json
│
├── src/                        ← Frontend SvelteKit
│   ├── app.css
│   ├── routes/
│   │   ├── +layout.svelte
│   │   └── +page.svelte        ← Vue principale
│   └── lib/
│       ├── components/         ← 14 composants UI (Rd*)
│       ├── managers/           ← 9 gestionnaires d'état
│       ├── api/                ← Clients API
│       ├── types/              ← Interfaces TypeScript
│       └── utils/              ← Helpers
│
├── RAPPORT_PFE_FINAL.md        ← Rapport PFE (71 Ko)
├── package.json
└── ANALYSE_PROJET.md           ← Ce document
```

---

## 4. Rôle Détaillé de Tauri 2.x (Rust + WebView)

Tauri est le **conteneur natif** qui transforme l'application web en application desktop installable (.exe, .msi).

### 4.1 Pourquoi Tauri plutôt qu'Electron ?

| Critère | Tauri | Electron |
|---------|-------|----------|
| Taille binaire | ~5–10 Mo | ~150 Mo |
| Moteur web | WebView2 natif Windows | Chromium embarqué |
| Backend | Rust (perf native) | Node.js |
| RAM | Faible | Élevée |
| Sécurité | Permissions granulaires | Globales |

### 4.2 Rôles précis de Tauri dans Lumière

#### a) Fenêtre native
Crée la fenêtre du système d'exploitation contenant la WebView qui affiche le frontend SvelteKit. Configuration dans `tauri.conf.json`.

#### b) Pont JavaScript ↔ Rust
Expose les fonctions Rust au frontend via `invoke()`. Exemples utilisés dans le projet :
```typescript
await invoke('start_agent', { sessionId, token });
await invoke('stop_agent');
await invoke('get_metrics');
await invoke('list_directory', { path });
```

#### c) Accès système privilégié (impossible en navigateur)
- **Capture d'écran** via DXGI ou Windows Graphics Capture
- **Contrôle souris/clavier** via la crate `enigo`
- **Exécution shell** via `tokio::process` (actions IA de type Shell)
- **Accès fichiers** : lecture de dossiers, écriture atomique de fichiers transférés
- **Métriques système** : CPU, RAM via la crate `sysinfo`

#### d) Hébergement du moteur WebRTC Rust
Côté agent, le peer connection WebRTC tourne **dans le backend Rust**, pas dans la WebView. Cela permet l'encodage H.264 hardware-accelerated et la capture écran à haute fréquence sans passer par les API navigateur limitées.

Voir : [src-tauri/src/agent/webrtc.rs](src-tauri/src/agent/webrtc.rs), [stream_senders.rs](src-tauri/src/agent/stream_senders.rs).

#### e) Sécurité et packaging
- Signature de code Windows
- Mises à jour automatiques (Tauri Updater)
- Permissions déclaratives dans `tauri.conf.json`
- Allowlist d'APIs exposées au frontend

> **Sans Tauri**, l'application ne pourrait ni capturer l'écran, ni contrôler la machine distante. C'est ce qui distingue Lumière d'une simple webapp.

---

## 5. Rôle Détaillé de SvelteKit / Svelte 5

Le frontend est l'**interface utilisateur** affichée dans la WebView de Tauri. Construit avec SvelteKit (framework) + Svelte 5 (compilateur réactif).

### 5.1 Pourquoi Svelte 5 ?

- **Compilation à la build** : pas de virtual DOM, code JS minimal et rapide
- **Runes** (`$state`, `$derived`, `$effect`) : réactivité fine et explicite
- **TypeScript natif**
- **Hot Module Replacement** via Vite
- **Bundle léger** : aligné avec la philosophie minimale de Tauri

### 5.2 Rôles précis du frontend

#### a) Interface utilisateur complète
Tous les écrans de l'application :
- Connexion / saisie code de session
- Visualisation du flux vidéo distant
- Chat temps réel
- Navigateur de fichiers distants
- Métriques système
- Modal d'approbation d'accès
- Historique sessions / transferts

#### b) Client WebRTC côté viewer (technicien)
Côté technicien, c'est le **frontend dans la WebView** qui :
- Reçoit le flux vidéo H.264
- Le décode (via le navigateur)
- L'affiche dans un `<video>`
- Capture les inputs souris/clavier et les renvoie via DataChannel
- Gère le chat, les fichiers, les actions IA

Voir : [src/lib/managers/viewer-peer.svelte.ts](src/lib/managers/viewer-peer.svelte.ts) (~46 Ko, plus gros fichier du projet).

#### c) State management réactif (Svelte 5 runes)
Les managers (`.svelte.ts`) utilisent `$state` et `$derived` pour exposer un état réactif partagé entre composants. Pattern « store moderne ».

#### d) Communication réseau « soft »
- **WebSocket** vers Spring Boot (signaling)
- **AiRealtimeClient** vers le service IA
- **API REST** vers Spring Boot pour l'historique et l'auth

#### e) Build
**Vite** compile le frontend en assets statiques (HTML/JS/CSS) que Tauri embarque dans le binaire final.

---

## 6. Collaboration Tauri ↔ Frontend

```
┌───────────────────────────────────────────────────────┐
│              FENÊTRE TAURI (binaire .exe)             │
│                                                       │
│  ┌─────────────────────────────────────────────────┐ │
│  │   WebView (WebView2 sur Windows)                │ │
│  │                                                 │ │
│  │   ┌───────────────────────────────────────┐    │ │
│  │   │  SvelteKit Application                │    │ │
│  │   │  - Composants Rd* (UI)                │    │ │
│  │   │  - Managers (state réactif)           │    │ │
│  │   │  - WebRTC client (côté viewer)        │    │ │
│  │   │  - WebSocket signaling                │    │ │
│  │   └────────────┬──────────────────────────┘    │ │
│  └────────────────┼─────────────────────────────────┘ │
│                   │                                    │
│            invoke() / events                           │
│                   │                                    │
│  ┌────────────────▼─────────────────────────────────┐ │
│  │   Backend Rust (processus Tauri)                 │ │
│  │   - Capture écran (DXGI / WGC)                   │ │
│  │   - Encodage H.264                               │ │
│  │   - WebRTC peer (côté agent)                     │ │
│  │   - Exécution actions IA (enigo, shell)          │ │
│  │   - Transfert fichiers                           │ │
│  │   - Métriques système                            │ │
│  └──────────────────────────────────────────────────┘ │
└───────────────────────────────────────────────────────┘
```

### Répartition selon le rôle de l'utilisateur

- **Machine contrôlée (agent)** : utilise massivement le **backend Rust** (capture, encodage, exécution actions IA, contrôle souris/clavier)
- **Machine du technicien (viewer)** : utilise surtout le **frontend Svelte** (réception vidéo, envoi inputs, UI chat/fichiers/IA)

**Le même binaire** joue les deux rôles selon le scénario — c'est l'un des 5 piliers techniques.

---

## 7. Backend Rust — Modules de l'Agent

Le dossier [src-tauri/src/agent/](src-tauri/src/agent/) contient **18 modules** spécialisés.

| Module | Responsabilité |
|--------|---------------|
| [mod.rs](src-tauri/src/agent/mod.rs) | Exports et orchestration des sous-modules |
| [webrtc.rs](src-tauri/src/agent/webrtc.rs) | Peer connection WebRTC, RTCP feedback, profils qualité (FpsTier, StreamQualityProfile), SSRC, timeout ICE |
| [adaptive_streaming.rs](src-tauri/src/agent/adaptive_streaming.rs) | Rate control, feedback RTCP, échantillonnage CPU, sélection profil qualité |
| [stream_senders.rs](src-tauri/src/agent/stream_senders.rs) | 3 boucles d'encodage : OpenH264 (Rust pur), MediaFoundation (HW Windows), FFmpeg (sidecar RTP) |
| [screen_capture.rs](src-tauri/src/agent/screen_capture.rs) | Capture via DXGI ou WGC, normalisation frames, scaling |
| [capture/](src-tauri/src/agent/capture/) | Trait `Capturer` + backends DXGI / WGC |
| [h264_helpers.rs](src-tauri/src/agent/h264_helpers.rs) | Parsing NAL units, réordonnancement SPS/PPS, packetisation RTP |
| [video_encoder.rs](src-tauri/src/agent/video_encoder.rs) | Trait `VideoEncoderBackend`, sélection preset (Auto, SoftwareOnly…) |
| [media_foundation_encoder.rs](src-tauri/src/agent/media_foundation_encoder.rs) | Worker encodeur H.264 MediaFoundation (Windows) |
| [desktop_duplication.rs](src-tauri/src/agent/desktop_duplication.rs) | API bas niveau Desktop Duplication |
| [file_channel_handler.rs](src-tauri/src/agent/file_channel_handler.rs) | DataChannel fichiers : listing, upload chunké, download, **screenshots IA** |
| [file_transfer.rs](src-tauri/src/agent/file_transfer.rs) | Service abstrait de transfert |
| [ai_executor.rs](src-tauri/src/agent/ai_executor.rs) | Réception AI_ACTION/AI_PLAN, dénormalisation coords, exécution via `enigo` + `tokio::process`, screenshot de vérification |
| [input_handler.rs](src-tauri/src/agent/input_handler.rs) | Streaming temps réel souris/clavier depuis viewer |
| [signaling.rs](src-tauri/src/agent/signaling.rs) | Client WebSocket vers Spring Boot |
| [session.rs](src-tauri/src/agent/session.rs) | Cycle de vie session, polling métriques, heartbeat |
| [metrics.rs](src-tauri/src/agent/metrics.rs) | Collecte métriques système (CPU, RAM…) |
| [auth.rs](src-tauri/src/agent/auth.rs) | Authentification, `PendingSession` DTO |
| [ice_servers.rs](src-tauri/src/agent/ice_servers.rs) | Résolution serveurs ICE (Metered en fallback, override via `LUMIERE_ICE_SERVERS`) |

---

## 8. Frontend — Composants UI

Dossier [src/lib/components/](src/lib/components/). Tous préfixés `Rd*` (Remote Desktop).

| Composant | Rôle |
|-----------|------|
| [RdAppHeader.svelte](src/lib/components/RdAppHeader.svelte) | En-tête avec branding et statut |
| [RdConnectPanel.svelte](src/lib/components/RdConnectPanel.svelte) | Saisie du code de session |
| [RdScreenPanel.svelte](src/lib/components/RdScreenPanel.svelte) | Affichage du flux vidéo distant + capture inputs |
| [RdViewerStatsBar.svelte](src/lib/components/RdViewerStatsBar.svelte) | Stats live (FPS, Mbps, RTT, packet loss) |
| [RdChatPanel.svelte](src/lib/components/RdChatPanel.svelte) | Panneau latéral chat |
| [RdChatList.svelte](src/lib/components/RdChatList.svelte) | Liste messages + auto-scroll |
| [RdChatCompose.svelte](src/lib/components/RdChatCompose.svelte) | Zone de saisie |
| [RdFilesPanel.svelte](src/lib/components/RdFilesPanel.svelte) | Navigateur de fichiers distants |
| [RdTransferList.svelte](src/lib/components/RdTransferList.svelte) | Liste transferts en cours |
| [RdMetricsPanel.svelte](src/lib/components/RdMetricsPanel.svelte) | CPU / RAM / disque |
| [RdSessionMenu.svelte](src/lib/components/RdSessionMenu.svelte) | Menu session (déconnexion, settings) |
| [RdApprovalModal.svelte](src/lib/components/RdApprovalModal.svelte) | Modal d'autorisation d'accès distant |
| [RdSessionHistory.svelte](src/lib/components/RdSessionHistory.svelte) | Historique sessions + recherche |
| [RdFileHistory.svelte](src/lib/components/RdFileHistory.svelte) | Historique transferts |

---

## 9. Frontend — Managers d'État

Dossier [src/lib/managers/](src/lib/managers/). Utilisent les **runes Svelte 5** (`$state`, `$derived`).

| Manager | Rôle |
|---------|------|
| [viewer-peer.svelte.ts](src/lib/managers/viewer-peer.svelte.ts) | **Le plus gros (~46 Ko)** — Peer WebRTC + DataChannels (inputs, fichiers, IA, chat) |
| [agent-manager.svelte.ts](src/lib/managers/agent-manager.svelte.ts) | Cycle de vie agent backend (start/stop), polling statut |
| [session-manager.svelte.ts](src/lib/managers/session-manager.svelte.ts) | État session, cycle connexion |
| [chat-manager.svelte.ts](src/lib/managers/chat-manager.svelte.ts) | Historique messages, notifications push |
| [file-channel.svelte.ts](src/lib/managers/file-channel.svelte.ts) | Bridge transfert fichiers ↔ DataChannel |
| [ai-pipeline.svelte.ts](src/lib/managers/ai-pipeline.svelte.ts) | AiRealtimeClient, orchestration actions IA, chunking screenshots, cooldown anti-spam (6 s) |
| [approval-manager.svelte.ts](src/lib/managers/approval-manager.svelte.ts) | État du modal d'approbation |
| [history-manager.svelte.ts](src/lib/managers/history-manager.svelte.ts) | Fetch historique sessions et transferts |
| [signal-bus.svelte.ts](src/lib/managers/signal-bus.svelte.ts) | Event bus inter-composants |

---

## 10. Communication WebRTC et Signaling

### 10.1 Établissement de la connexion

```
AGENT                  SPRING BOOT              VIEWER
  │                         │                      │
  │── WS connect ──────────►│◄────── WS connect ───┤
  │                         │                      │
  │                         │      offer SDP       │
  │◄────────────────────────┤◄─────────────────────┤
  │                         │                      │
  │── answer SDP ──────────►│─────────────────────►│
  │                         │                      │
  │── ICE candidates ──────►│─────────────────────►│
  │◄────────────────────────┤◄─────────────────────┤
  │                         │                      │
  │═══════════ Connexion P2P WebRTC ═══════════════│
  │           (vidéo + DataChannels)               │
```

### 10.2 Serveurs ICE

- **Fallback hardcodé** : Metered (STUN + TURN)
- **Override** via variable d'environnement `LUMIERE_ICE_SERVERS`
- Géré par [ice_servers.rs](src-tauri/src/agent/ice_servers.rs)

### 10.3 DataChannels utilisés

| Channel | Sens | Contenu |
|---------|------|---------|
| Input | Viewer → Agent | Événements souris/clavier |
| File | Bidirectionnel | Listing, upload chunké, download, screenshots IA |
| AI | Viewer → Agent | AI_ACTION, AI_PLAN |
| AI Result | Agent → Viewer | Résultats d'exécution + screenshots verif |
| Chat | Bidirectionnel | Messages utilisateurs |
| Control | Bidirectionnel | Heartbeat, contrôle session |

### 10.4 Feedback RTCP

- **NACK** : retransmission paquets perdus
- **PLI** (Picture Loss Indication) : demande keyframe
- **FIR** (Full Intra Request) : keyframe forcé
- Adaptation dynamique FPS et bitrate via [adaptive_streaming.rs](src-tauri/src/agent/adaptive_streaming.rs)

---

## 11. Capture d'Écran et Encodage Vidéo

### 11.1 Backends de capture

Sélectionnable via `LUMIERE_CAPTURE_BACKEND` :

| Backend | Description | Avantage |
|---------|-------------|----------|
| **DXGI** | Desktop Duplication API | Standard, compatible large |
| **WGC** | Windows Graphics Capture | Moderne, exclut overlays sensibles |

Sélection du moniteur via `LUMIERE_CAPTURE_MONITOR_INDEX` (0-based).

### 11.2 Encodeurs H.264

| Encodeur | Type | Usage |
|----------|------|-------|
| **OpenH264** | Logiciel (Rust pur) | Portable, fallback |
| **MediaFoundation** | Hardware Windows | Performant, GPU |
| **FFmpeg** | Sidecar RTP | Flexibilité maximale |

Sélection via le preset `VideoEncoderBackend` : `Auto`, `SoftwareOnly`, etc.

### 11.3 Pipeline complet

```
[Écran] → DXGI/WGC → [Frame BGRA] → Normalize/Scale
       → Encoder H.264 → NAL units → RTP packets
       → WebRTC → Réseau P2P → Viewer
       → <video> HTML5 (décodage par WebView2)
```

### 11.4 Adaptation dynamique

- Échantillonnage CPU en continu
- Lecture du feedback RTCP (perte de paquets, RTT)
- Sélection automatique du profil qualité (Low / Medium / High)
- Ajustement FPS et bitrate cible

---

## 12. Intégration de l'Intelligence Artificielle

### 12.1 Flux global

```
Viewer (technicien)
  │
  │  1. Capture screenshot via DataChannel "File"
  │     (handle_screenshot_request côté agent → JPEG base64 chunké)
  ▼
[Screenshot reçu]
  │
  │  2. Envoi à AiRealtimeClient (WebSocket service IA)
  ▼
[Service IA Realtime]
  │
  │  3. Retourne AI_PLAN ou AI_ACTION
  │     (coordonnées normalisées 0–1)
  ▼
Viewer → DataChannel AI → Agent
  │
  │  4. ai_executor.rs côté agent :
  │     - Dénormalise les coords (× résolution écran)
  │     - Exécute action :
  │         • Click / TypeText / Key / Scroll / Drag → enigo
  │         • Shell → tokio::process
  │         • Screenshot → file_channel_handler
  ▼
[Action exécutée]
  │
  │  5. Screenshot de vérification capturé
  │  6. Résultat JSON renvoyé sur DataChannel
  ▼
Viewer (affichage résultat dans chat)
```

### 12.2 Types d'actions supportées

| Action | Mécanisme |
|--------|-----------|
| **Click** | `enigo` (gauche, droit, double) |
| **TypeText** | `enigo` (frappe clavier) |
| **Key** | `enigo` (touches spéciales, raccourcis) |
| **Scroll** | `enigo` |
| **Drag** | `enigo` (down + move + up) |
| **Shell** | `tokio::process::Command` |
| **Screenshot** | Capture frame + JPEG + base64 chunké |

### 12.3 Sécurité et fiabilité

- **Cooldown anti-spam** : 6 secondes minimum entre 2 commandes IA (côté frontend, [ai-pipeline.svelte.ts](src/lib/managers/ai-pipeline.svelte.ts))
- **Vérification post-action** : screenshot capturé pour confirmer l'effet
- **Coordonnées normalisées** : indépendance résolution
- **Résultats structurés** : JSON renvoyé au viewer pour affichage chat

---

## 13. Transfert de Fichiers

### 13.1 Protocole sur DataChannel

| Message | Sens | Rôle |
|---------|------|------|
| `FILE_LIST_REQUEST` | Viewer → Agent | Lister un dossier distant |
| `FILE_LIST_RESPONSE` | Agent → Viewer | Contenu du dossier |
| `FILE_DOWNLOAD_REQUEST` | Viewer → Agent | Télécharger un fichier |
| `FILE_UPLOAD_START` | Viewer → Agent | Début upload (métadonnées) |
| Binary chunks | Viewer ↔ Agent | Données |
| `FILE_COMPLETE` | Fin transfert | Validation |

### 13.2 Côté agent ([file_channel_handler.rs](src-tauri/src/agent/file_channel_handler.rs))

- Accumulation en mémoire
- Écriture atomique à la fin (évite races antivirus)
- Logging des transferts vers backend Spring Boot

### 13.3 UI

- [RdFilesPanel.svelte](src/lib/components/RdFilesPanel.svelte) : navigation
- [RdTransferList.svelte](src/lib/components/RdTransferList.svelte) : transferts actifs
- [RdFileHistory.svelte](src/lib/components/RdFileHistory.svelte) : historique

---

## 14. Chat Temps Réel

- **Transport** : WebSocket via `chat-realtime.ts` (et/ou DataChannel selon le contexte)
- **Persistance** : historique local + backend
- **Notifications push** : signalement nouveaux messages
- **Intégration IA** : les messages IA et résultats d'actions apparaissent dans le chat
- **Indicateurs** : typing indicators

Gestionnaire : [chat-manager.svelte.ts](src/lib/managers/chat-manager.svelte.ts).

---

## 15. État Actuel du Développement

### 15.1 Derniers commits

| SHA | Description |
|-----|-------------|
| `6139c92` | Modal d'approbation accès distant + historique transferts |
| `4c1003f` | Actions scroll & drag dans handler IA |
| `2fd2a34` | Version 1 IA |
| `5d02ab1` | Gestion requêtes screenshot distant |
| `1392b3a` | Cooldown anti-spam IA + amélioration gestion erreurs |

### 15.2 Travaux en cours (non commités)

**Refactoring** : extraction du gros fichier `webrtc.rs` en modules dédiés :
- `adaptive_streaming.rs` (nouveau)
- `stream_senders.rs` (nouveau)
- `file_channel_handler.rs` (nouveau)
- `h264_helpers.rs` (nouveau)
- `ice_servers.rs` (nouveau)

**Nouveaux fichiers UI** (~20) :
- Tous les composants `Rd*` dans `src/lib/components/`
- Tous les managers dans `src/lib/managers/`
- `src/routes/+layout.svelte`
- `src/app.css`

**Document** : `RAPPORT_PFE_FINAL.md` (71 Ko)

---

## 16. Synthèse des 5 Piliers Techniques

D'après le rapport PFE :

1. **Application desktop unifiée multi-rôle**
   Un seul binaire léger (Tauri + Rust) distribué sur tous les postes, jouant alternativement le rôle agent ou viewer.

2. **Backend Spring Boot central**
   Authentification, signaling WebSocket, orchestration sessions, traçabilité, historique.

3. **Canal temps réel P2P WebRTC**
   Vidéo + DataChannels (inputs, fichiers, IA, chat), traversal NAT via STUN/TURN.

4. **Capture & encodage H.264 performant**
   DXGI/WGC + 3 encodeurs (OpenH264 / MediaFoundation / FFmpeg) avec adaptation dynamique.

5. **Couche d'intelligence artificielle**
   Analyse de screenshots + exécution d'actions guidées avec vérification automatique.

---

## Conclusion

**Lumière Tech-IT** est un projet **mature et bien architecturé** qui combine :

- **Performance native** (Rust + Tauri pour la capture/encodage)
- **UI moderne et réactive** (Svelte 5 + TypeScript)
- **Communication temps réel optimale** (WebRTC P2P + WebSocket signaling)
- **Intelligence artificielle pratique** (actions vérifiables, anti-spam, screenshots)
- **Sécurité et gouvernance** (modal d'approbation, historique, auth centralisée)

La modularité du code (18 modules Rust, 14 composants Svelte, 9 managers) témoigne d'une attention particulière à la maintenabilité, et la stratégie multi-encodeur garantit la portabilité et la performance sur différentes configurations matérielles.

---

*Document généré dans le cadre de l'analyse du projet PFE Lumière Tech-IT.*

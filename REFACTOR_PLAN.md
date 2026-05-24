# Plan de Refactoring — Lumière Tech-IT

> Document de stratégie pour les refactorings de plus de 4h chacun.
> Les améliorations ponctuelles ont déjà été appliquées (voir [AMELIORATIONS.md](AMELIORATIONS.md)).

---

## 🟢 Refactorings DÉJÀ EFFECTUÉS

### 1. Migration `println!`/`eprintln!` → `tracing` ✅
- **Fait** : 219 appels remplacés sur 14 fichiers Rust
- **Bénéfices** :
  - Filtrage runtime via `RUST_LOG=lumiere_tech_it_lib=debug`
  - Subscriber initialisé dans `lib.rs::run()`
  - Compilation OK, 6/6 tests passent
- **Suite possible** : ajuster les niveaux (info → debug pour les frame stats)

### 2. Factorisation boucle RTP encoders ✅
- **Fait** : extraction de la fonction `emit_nal_as_rtp` + struct `RtpEmitCtx`
- **Avant** : ~80 LOC dupliqués dans `run_openh264_screen_sender` et `run_media_foundation_screen_sender`
- **Après** : un helper unique de 60 LOC + 4 appels de ~10 LOC
- **Gain** : ~100 LOC supprimés, sémantique préservée (tests OK)

### 3. Extraction constantes + types viewer-peer ✅
- **Fait** : création de `viewer-peer.constants.ts` et `viewer-peer.types.ts`
- **Gain** : ~30 LOC sortis de la classe monstre + types réutilisables

---

## 🟡 Refactoring complet de `viewer-peer.svelte.ts` (TODO)

### État actuel
- **Fichier** : `src/lib/managers/viewer-peer.svelte.ts` — **1359 LOC** (était 1396 avant l'extraction des constantes)
- **Structure** : une classe `ViewerPeer` avec ~50 propriétés `$state` réactives et ~40 méthodes

### Problème
La classe couple 6 responsabilités distinctes :
1. **Connexion WebRTC** (peer, signaling, ICE) — ~400 LOC
2. **DataChannels** (control, input) — ~200 LOC
3. **Inputs utilisateur** (clavier/souris/molette) — ~200 LOC
4. **Stats live** (FPS, Mbps, RTT, perte) — ~150 LOC
5. **Profils / presets** (responsive/quality, FPS tier, bitrate tier) — ~150 LOC
6. **UI** (controls visibility, fullscreen, chat panel) — ~100 LOC

### Stratégie recommandée

#### Étape 1 — Extraire les sous-managers (3-4 h)

Créer 4 nouvelles classes, chacune avec son propre fichier `.svelte.ts` :

```
src/lib/managers/viewer/
├── viewer-stats.svelte.ts         # ViewerStatsManager
│   - viewerStreamMbps, viewerStreamFps, viewerLocal*
│   - inboundStatsTimer + collectStats()
│
├── viewer-input.svelte.ts         # ViewerInputManager
│   - viewerKeyboardCaptured
│   - last*SentAt throttling state
│   - handle{Key,Pointer,Wheel}* methods
│
├── viewer-profile.svelte.ts       # ViewerProfileManager
│   - viewerPlaybackProfile, viewerFpsTier, viewerBitrateTier, viewerPreset
│   - applyViewerPreset, maybeAutoUpgradeViewerProfile, etc.
│
└── viewer-ice.svelte.ts           # ViewerIceManager
    - viewerPeerConnection, viewerControlChannel
    - ICE candidates, restart logic, watchdog
```

Chaque manager :
- A son propre cycle de vie (`start()` / `destroy()`)
- Reçoit les dépendances via constructor / setters
- Expose son état via `$state` typé

#### Étape 2 — Réduire `ViewerPeer` à un orchestrateur (1-2 h)

```typescript
export class ViewerPeer {
  stats = new ViewerStatsManager();
  input = new ViewerInputManager();
  profile = new ViewerProfileManager();
  ice = new ViewerIceManager();

  // UI-only state (fullscreen, controls visibility)
  // + Callbacks (configureFileDataChannel, etc.)

  resetViewerPeerConnection() {
    this.stats.destroy();
    this.input.destroy();
    this.profile.destroy();
    this.ice.destroy();
  }
}
```

#### Étape 3 — Mettre à jour les consommateurs (1-2 h)

Composants qui accèdent à `viewerPeer.xxx` à migrer :
- `+page.svelte`
- `RdScreenPanel.svelte`
- `RdViewerStatsBar.svelte`
- `RdSessionMenu.svelte`
- `ai-pipeline.svelte.ts` (réf. à `viewerControlChannel`, `viewerVideoEl`)

**Approche safe** : ajouter d'abord des getters de compatibilité sur `ViewerPeer` qui délèguent aux sub-managers, puis migrer les consommateurs un par un.

```typescript
class ViewerPeer {
  // Backward-compat getters (à supprimer après migration)
  get viewerStreamMbps() { return this.stats.viewerStreamMbps; }
  get viewerControlChannel() { return this.ice.controlChannel; }
  // ...
}
```

#### Risques

- **État partagé** : certaines méthodes consultent l'état de 3-4 propriétés à la fois (ex: `resetViewerPeerConnection` touche tout). Nécessite de bien tracer les dépendances.
- **Timing** : l'ordre d'initialisation/destruction des managers est critique (l'ICE doit fermer le peer AVANT que les stats s'arrêtent).
- **Svelte 5 runes** : tester que les `$state` cross-manager se propagent correctement (les composants observent souvent plusieurs champs).

#### Tests à ajouter avant le refactor

- Test E2E : connexion viewer-agent → vérifier stats visibles
- Test : déconnexion en cours de stream → tous les timers stoppés
- Test : ICE restart manuel → peer recréé sans fuite
- Test : preset change → profile signaling envoyé

---

## 🟡 Autres refactorings >4h non effectués

### A. Fusion des fonctions `run_*_screen_sender` (ouvrir un sprint)

**Fichier** : `src-tauri/src/agent/stream_senders.rs`

**État** : la boucle RTP a déjà été factorisée (étape 2 ci-dessus). Restent ~400 LOC dupliqués entre `run_openh264_*` et `run_media_foundation_*` :
- Init encoder
- Capture loop avec frame skipping
- Stats reporting

**Stratégie** : passer un trait object `Box<dyn VideoEncoderBackend>` à une fonction `run_screen_sender` unique. Le trait existe déjà — il manque juste les méthodes pour exposer le format de sortie (Annex-B chunks).

### B. Streaming upload vers un fichier `.part`

**Fichier** : `src-tauri/src/agent/file_channel_handler.rs`

**Pourquoi non fait** : le projet a choisi explicitement d'accumuler en RAM avant `FILE_COMPLETE` pour éviter les races antivirus / Defender (commentaire dans le code).

**Compromis** : écrire dans `<dest>.part` au fur et à mesure, puis `rename` atomique en fin. Le `.part` peut être ignoré via une règle `.gitignore`-style.

### C. Tests unitaires backend Rust

**État** : seul `metrics.rs` a des tests (6). 17 modules sans tests.

**Priorité** :
1. `h264_helpers.rs` (parsing NAL, easy)
2. `ice_servers.rs` (parsing env var)
3. `ai_executor.rs` (dénormalisation des coords)
4. `adaptive_streaming.rs` (rate controller logic)

**Effort estimé** : 1-2 jours pour atteindre 50% de couverture.

### D. Couche Tokio explicite

**Fichier** : `src-tauri/Cargo.toml`

**Actuel** : `tokio = { features = ["full"] }` (64 sous-features)

**Cible** :
```toml
tokio = { version = "1", features = [
  "rt-multi-thread", "macros", "time", "sync",
  "io-util", "process", "net", "fs", "signal"
] }
```

**Risque** : compilation peut échouer subtilement si une feature implicite est utilisée. Nécessite un test E2E complet.

---

## 📊 Bilan global

| Refactoring | État | Effort restant | Gain estimé |
|-------------|------|---------------|-------------|
| `println!` → `tracing` | ✅ Fait | 0 | Logs filtrables |
| Boucle RTP factorisée | ✅ Fait | 0 | -100 LOC |
| Constantes/types viewer | ✅ Fait | 0 | -30 LOC |
| Split viewer-peer.svelte | 🟡 Stratégie écrite | 6-8 h | -800 LOC répartis |
| Fusion encoders | 🟡 Stratégie écrite | 4-6 h | -400 LOC |
| Upload streaming `.part` | 🟡 Stratégie écrite | 2-3 h | RAM -800 MB |
| Tests backend | 🟡 Stratégie écrite | 1-2 j | Couverture 0% → 50% |
| Tokio features | 🟡 Stratégie écrite | 1-2 h | Bundle -2 MB |

**Total** : ~3-5 jours de travail concentré pour finir les refactorings non triviaux.

---

*Ce document remplace les estimations grossières d'AMELIORATIONS.md par un plan d'exécution concret. Les bénéfices immédiats (sécurité, perf hot-path, mémoire) ont déjà été livrés.*

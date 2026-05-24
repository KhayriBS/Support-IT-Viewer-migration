# Améliorations, Corrections et Optimisations — Lumière Tech-IT

> Audit technique complet du codebase Tauri / Rust / Svelte
> Identifié à partir de l'analyse approfondie des fichiers du projet

---

## Synthèse Exécutive

Le code est **globalement solide** : bonne structure WebRTC, gestion des timeouts, fallbacks d'encodeurs, modularité. Cependant l'audit révèle :

- **3 vulnérabilités de sécurité réelles** (path traversal, shell injection, CSP désactivée)
- **2 memory leaks** côté frontend (timers et listeners non nettoyés)
- **~10 problèmes de performance** sur les hot paths (clones inutiles, allocations en boucle)
- **Du code dupliqué** entre les boucles d'encodage (~600 LOC à factoriser)
- **Quelques `unwrap()`** qui peuvent crasher l'agent en production

Priorisation : **CRITIQUE** (à corriger avant production) → **IMPORTANT** (à corriger avant release) → **NICE-TO-HAVE** (post-MVP).

---

## 🔴 CRITIQUE — Risques de crash / sécurité / corruption

### 1. Path Traversal dans le téléchargement de fichiers
**Fichier** : [src-tauri/src/agent/file_channel_handler.rs:203-206](src-tauri/src/agent/file_channel_handler.rs#L203)
**Problème** : `safe_name` est utilisé sans validation contre les séquences `..` ou les symlinks. Un attaquant qui contrôle le viewer peut potentiellement lire des fichiers hors du dossier prévu.
**Solution** :
```rust
let canonical = path.canonicalize()?;
if !canonical.starts_with(&allowed_root) {
    return Err("Path traversal détecté");
}
```

### 2. Path Traversal dans l'upload de fichiers
**Fichier** : [src-tauri/src/agent/file_channel_handler.rs:317](src-tauri/src/agent/file_channel_handler.rs#L317)
**Problème** : `PathBuf::from(&dest)` accepte un chemin absolu non validé. L'upload peut écrire **n'importe où** sur le disque (y compris `C:\Windows\System32\`).
**Solution** : Imposer que `dest` reste contenu dans `LumiereTransfers/` via `canonicalize()` + `starts_with()`.

### 3. Shell Command Injection
**Fichier** : [src-tauri/src/agent/ai_executor.rs:334-345](src-tauri/src/agent/ai_executor.rs#L334)
**Problème** : Si une action IA de type `Shell` contient des guillemets non échappés, injection possible.
**Solution** : Utiliser `Command::new()` avec `argv` array (pas de shell intermédiaire), valider strictement la commande, ou désactiver par défaut.

### 4. Content Security Policy désactivée
**Fichier** : [src-tauri/tauri.conf.json:21](src-tauri/tauri.conf.json#L21)
**Problème** : `"csp": null` désactive complètement la CSP. Injection JS possible si une source non-sûre s'introduit.
**Solution** : Définir une CSP minimale même en développement :
```json
"csp": "default-src 'self'; img-src 'self' data:; connect-src 'self' ws: wss: https:"
```

### 5. `unwrap()` qui peut crasher l'agent
**Fichier** : [src-tauri/src/agent/webrtc.rs:85](src-tauri/src/agent/webrtc.rs#L85)
**Problème** : `derive_stream_ssrc()` peut paniquer si la durée système est invalide.
**Solution** : Utiliser `Duration::ZERO` par défaut au lieu de paniquer.

### 6. `expect()` sur l'init d'Enigo
**Fichier** : [src-tauri/src/agent/ai_executor.rs:168](src-tauri/src/agent/ai_executor.rs#L168)
**Problème** : Crash de l'agent si le contrôle des entrées Windows est bloqué (UAC, politique groupe).
**Solution** : Retourner une `Result::Err` gracieuse et notifier le viewer.

### 7. Code de sortie process masqué
**Fichier** : [src-tauri/src/agent/ai_executor.rs:363](src-tauri/src/agent/ai_executor.rs#L363)
**Problème** : `.unwrap_or(-1)` masque les vrais codes d'erreur.
**Solution** : `output.status.code().unwrap_or_else(|_| 1)` ou propager l'erreur.

### 8. Allowlist de dossiers manquante
**Fichier** : [src-tauri/src/agent/file_channel_handler.rs:181](src-tauri/src/agent/file_channel_handler.rs#L181)
**Problème** : `get_directory_listing()` accepte n'importe quel chemin, y compris `C:\Windows\System32\config\`.
**Solution** : Ajouter une allowlist (Downloads, Desktop, Documents, lecteurs configurés).

---

## 🟠 IMPORTANT — Performance / Memory leaks / Robustesse

### Performance Backend Rust

### 9. Clones inutiles dans la hot loop RTP
**Fichier** : [src-tauri/src/agent/stream_senders.rs:556, 602, 1104, 1150](src-tauri/src/agent/stream_senders.rs#L556)
**Problème** : 3× `.clone()` sur `fragment` à chaque paquet RTP — coûteux à 60 FPS.
**Solution** : Utiliser `Bytes::copy_from_slice()` ou `Arc<Bytes>` pour partage zero-copy.

### 10. Allocations Vec sans capacity
**Fichier** : [src-tauri/src/agent/stream_senders.rs:510, 1059](src-tauri/src/agent/stream_senders.rs#L510)
**Problème** : `Vec::new()` à chaque frame H264 — réallocations multiples.
**Solution** : `Vec::with_capacity(8)` (max ~8 NAL units par frame).

### 11. Double allocation lors du packetizing
**Fichier** : [src-tauri/src/agent/stream_senders.rs:478](src-tauri/src/agent/stream_senders.rs#L478)
**Problème** : `.payload(1200, &nal_bytes)` copie le NAL complet.
**Solution** : Réutiliser un buffer pré-alloué.

### 12. `spawn_blocking` à chaque frame
**Fichier** : [src-tauri/src/agent/stream_senders.rs:369, 812](src-tauri/src/agent/stream_senders.rs#L369)
**Problème** : `tokio::task::spawn_blocking()` en boucle pour la conversion BGRA→YUV — overhead.
**Solution** : Pool de threads dédié ou exécution synchrone si la conversion est rapide.

### 13. `System::new_all()` répété
**Fichier** : [src-tauri/src/agent/adaptive_streaming.rs:266, 269](src-tauri/src/agent/adaptive_streaming.rs#L266)
**Problème** : Création complète de `sysinfo::System` à chaque sampler — coûteux.
**Solution** : Instance globale lazy-initialisée (`OnceLock<Mutex<System>>`).

### 14. Lock tenu pendant await
**Fichier** : [src-tauri/src/agent/webrtc.rs:517-524](src-tauri/src/agent/webrtc.rs#L517)
**Problème** : Mutex maintenu pendant `.await` sur `add_ice_candidate()` — blocking sous contention.
**Solution** : Snapshot des données nécessaires, libération du lock, puis await.

### 15. Channels broadcast non bornés
**Fichier** : [src-tauri/src/agent/stream_senders.rs:225, 721](src-tauri/src/agent/stream_senders.rs#L225)
**Problème** : `broadcast::channel(2)` mais publication non bornée si encoding lag — memory pressure.
**Solution** : Mesurer et limiter à 8 max, drop old frames.

### Memory Leaks Frontend

### 16. Intervals non clearés
**Fichier** : [src/lib/managers/viewer-peer.svelte.ts:125-145](src/lib/managers/viewer-peer.svelte.ts#L125)
**Problème** : `setInterval()` pour stats sans `clearInterval()` visible. Reconnexions = accumulation de timers.
**Solution** : Stocker l'ID, clear dans la méthode `destroy()` / `disconnect()`.

### 17. Timers multiples sans cleanup centralisé
**Fichier** : [src/lib/managers/viewer-peer.svelte.ts:256, 309, 451, 538, 650](src/lib/managers/viewer-peer.svelte.ts#L256)
**Problème** : 5× `setTimeout`/`setInterval` sans garantie de nettoyage à la fermeture de session.
**Solution** : Centraliser via un `Set<NodeJS.Timeout>` purgé dans `destroy()`.

### 18. Watchdog IA persistant
**Fichier** : [src/lib/managers/ai-pipeline.svelte.ts:43, 89, 186](src/lib/managers/ai-pipeline.svelte.ts#L43)
**Problème** : Timer watchdog reste armé après disconnect du WebSocket IA.
**Solution** : `clearTimeout()` dans le handler `onclose`.

### Accumulation mémoire

### 19. Fichiers accumulés intégralement en RAM
**Fichier** : [src-tauri/src/agent/file_channel_handler.rs:404](src-tauri/src/agent/file_channel_handler.rs#L404)
**Problème** : `.extend_from_slice()` accumule **tout le fichier en mémoire** avant `FILE_COMPLETE`. Un upload de 1 GB = 1 GB de RAM.
**Solution** : Écrire en streaming sur disque (tmp file) puis renommer atomiquement.

### Logs et secrets

### 20. Secrets potentiellement loggés
**Fichier** : [src-tauri/src/agent/ai_executor.rs:520-529](src-tauri/src/agent/ai_executor.rs#L520)
**Problème** : Logs incluent potentiellement les payloads `AI_ACTION_RESULT` contenant des tokens.
**Solution** : Filtrer les champs sensibles avant log (allowlist de champs loggables).

### Robustesse

### 21. Pas de cleanup mid-transfer
**Fichier** : [src-tauri/src/agent/file_channel_handler.rs:427-513](src-tauri/src/agent/file_channel_handler.rs#L427)
**Problème** : `handle_file_download()` continue d'écrire même si la session se ferme.
**Solution** : Vérifier `channel.is_open()` à chaque chunk.

### 22. Reconnexion sans backoff
**Fichier** : [src/lib/managers/viewer-peer.svelte.ts:493-520](src/lib/managers/viewer-peer.svelte.ts#L493)
**Problème** : Offer retry loop à 60×100ms = flood du serveur signaling.
**Solution** : Backoff exponentiel (100ms, 200ms, 400ms, 800ms… plafonné).

### 23. Pas de cancellation pour actions IA longues
**Fichier** : [src-tauri/src/agent/ai_executor.rs:382-386](src-tauri/src/agent/ai_executor.rs#L382)
**Problème** : `do_wait()` + `do_screenshot()` non annulables — viewer ferme = encoding continue.
**Solution** : `tokio_util::sync::CancellationToken`.

### 24. Screenshot après session close
**Fichier** : [src-tauri/src/agent/webrtc.rs:346-351](src-tauri/src/agent/webrtc.rs#L346)
**Problème** : `tokio::spawn()` pour screenshot, réponse arrive parfois après session close.
**Solution** : Binder le spawn à un session_id, drop la réponse si invalide.

---

## 🟡 NICE-TO-HAVE — Code quality / Bundle / Maintenabilité

### 25. Duplication massive entre encodeurs
**Fichier** : [src-tauri/src/agent/stream_senders.rs:198-692, 694-1249](src-tauri/src/agent/stream_senders.rs#L198)
**Problème** : ~600 LOC quasi identiques entre `run_openh264_screen_sender()` et `run_media_foundation_screen_sender()`.
**Solution** : Extraire une fonction `run_screen_sender<E: VideoEncoderBackend>()` générique.

### 26. Classe monstre côté frontend
**Fichier** : [src/lib/managers/viewer-peer.svelte.ts:38-89](src/lib/managers/viewer-peer.svelte.ts#L38)
**Problème** : 50+ propriétés réactives sur une classe unique (~46 Ko).
**Solution** : Découper en composables : `useViewerStats()`, `useViewerConnection()`, `useViewerInputs()`.

### 27. Fichier de 466 lignes sans décomposition
**Fichier** : [src-tauri/src/agent/file_channel_handler.rs:49-515](src-tauri/src/agent/file_channel_handler.rs#L49)
**Problème** : Fonction monolithique, impossible à tester unitairement.
**Solution** : Splitter en `handle_screenshot_request()`, `handle_upload()`, `handle_download()`, `handle_list()`.

### 28. Logs non structurés
**Fichier** : [src-tauri/src/agent/webrtc.rs:145](src-tauri/src/agent/webrtc.rs#L145)
**Problème** : `println!()` en production, pas de niveau, pas de filtrage.
**Solution** : Adopter `tracing` (déjà partiellement utilisé dans [adaptive_streaming.rs:117](src-tauri/src/agent/adaptive_streaming.rs#L117)) partout.

### 29. Macro `vlog!()` custom
**Fichier** : [src-tauri/src/agent/stream_senders.rs:623-641](src-tauri/src/agent/stream_senders.rs#L623)
**Problème** : Macro maison difficile à parser/filtrer.
**Solution** : Remplacer par `tracing::info!(target = "stream", ...)`.

### 30. Tokio en mode "full"
**Fichier** : [src-tauri/Cargo.toml](src-tauri/Cargo.toml)
**Problème** : `tokio = { features = ["full"] }` active 64 sous-dépendances inutilisées.
**Solution** : Lister explicitement :
```toml
tokio = { version = "1", features = ["rt-multi-thread", "macros", "time", "sync", "io-util", "process", "net"] }
```

### 31. Pas de tests
**Problème** : Aucun fichier `tests/` ou `#[cfg(test)]` significatif détecté.
**Solution** : Ajouter au minimum :
- Tests unitaires pour `h264_helpers.rs` (parsing NAL)
- Tests pour `ice_servers.rs` (parsing env var)
- Tests pour `ai_executor.rs` (dénormalisation coords)

### 32. Silent IO errors
**Fichier** : [src-tauri/src/agent/file_transfer.rs:127](src-tauri/src/agent/file_transfer.rs#L127)
**Problème** : `.flatten()` ignore les erreurs de `read_dir()` — un dossier inaccessible passe inaperçu.
**Solution** : Logger explicitement les erreurs (`warn!`).

### 33. `.expect()` divers
**Action** : Auditer tous les `.expect()` et `.unwrap()` du backend Rust :
```bash
rg --type rust '\.(unwrap|expect)\(' src-tauri/src/agent/
```
Remplacer par `?` propagation ou error logging.

### 34. Frame fallback silencieux
**Fichier** : [src-tauri/src/agent/stream_senders.rs:274](src-tauri/src/agent/stream_senders.rs#L274)
**Problème** : `capture_primary_screen_even_bgra()` peut retourner `None`, fallback silencieux à la frame précédente = artefacts visuels.
**Solution** : Logger la raison + métriques de fréquence de fallback.

### 35. Overflow potentiel
**Fichier** : [src-tauri/src/agent/file_channel_handler.rs:95](src-tauri/src/agent/file_channel_handler.rs#L95)
**Problème** : `div_ceil()` peut overflow sur très gros fichiers (>4 GB sur 32-bit).
**Solution** : `u64::div_ceil()` ou `(total_bytes + CHUNK_SIZE - 1) / CHUNK_SIZE` avec types u64.

---

## 📋 Plan d'Action Recommandé

### Phase 1 — Sécurité (avant toute démo client)
- ✅ Items **1, 2, 3, 4, 8** (path traversal, shell injection, CSP, allowlist dossiers)
- ✅ Item **20** (filtrer secrets dans logs)

### Phase 2 — Stabilité (avant release)
- ✅ Items **5, 6, 7** (unwrap/expect → Result propres)
- ✅ Items **16, 17, 18** (memory leaks frontend)
- ✅ Items **19, 21, 22, 23, 24** (robustesse runtime)

### Phase 3 — Performance (optimisation continue)
- ✅ Items **9, 10, 11, 12, 13, 14, 15** (hot paths Rust)

### Phase 4 — Qualité (refactor)
- ✅ Items **25, 26, 27** (décomposition)
- ✅ Items **28, 29** (logs structurés)
- ✅ Item **31** (tests)

### Phase 5 — Bundle (optimisation finale)
- ✅ Item **30** (Tokio features)
- ✅ Items **32-35** (polish)

---

## 🛠️ Quick Wins (faciles et rapides)

Les améliorations suivantes nécessitent peu d'effort mais apportent un gain immédiat :

1. **CSP minimale** dans `tauri.conf.json` (item #4) — 5 minutes
2. **`Vec::with_capacity`** dans les hot loops (item #10) — 10 minutes
3. **Backoff exponentiel** dans le retry signaling (item #22) — 15 minutes
4. **Tokio features explicites** (item #30) — 5 minutes + recompile
5. **`canonicalize() + starts_with()`** dans file_channel_handler (items #1, #2) — 30 minutes

**Gain estimé** : ~1 heure de travail pour fermer 4 vulnérabilités et améliorer le bundle.

---

## 📊 Métriques Cibles

Objectifs mesurables suite à ces optimisations :

| Métrique | Avant (estimé) | Cible |
|----------|---------------|-------|
| Taille binaire (release) | ~12 MB | < 8 MB |
| RAM agent en idle | ~80 MB | < 50 MB |
| RAM agent upload 1 GB | ~1 GB | < 100 MB |
| CPU agent en streaming | ~25% | < 15% |
| Latence d'action IA | ~500ms | < 300ms |
| Allocations / seconde (hot path RTP) | élevé | -50% |

---

## 🔍 Outils Recommandés

Pour automatiser la détection de ces problèmes :

- **`cargo clippy -- -D warnings`** : lints Rust
- **`cargo audit`** : CVEs dans dépendances
- **`cargo deny`** : politique sur les licences/deps
- **`cargo flamegraph`** : profiling CPU
- **`cargo udeps`** : dépendances inutilisées
- **`tokio-console`** : observation runtime async
- **`svelte-check`** : erreurs TypeScript/Svelte
- **Chrome DevTools Performance** : profiler la WebView

---

*Audit réalisé sur le snapshot du code à la date du rapport. Les numéros de ligne peuvent évoluer avec les commits suivants.*

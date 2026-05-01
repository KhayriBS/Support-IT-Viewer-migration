# Plan: Remote Desktop Production Hardening

Date: 2026-04-21

## Objectif validé
- Supprimer `FileData` pour la vidéo.
- Garder WebRTC-only pour le flux écran.
- Conserver le transfert de fichiers.
- Livrer en phases sûres avec validation continue.

## Décisions prises
- `FileData` vidéo: supprimé.
- `FileData` fichiers (upload/download): conservé.
- Preview legacy image: supprimé (WebRTC uniquement).
- Capture: DXGI + WGC fallback.
- Adaptation: CPU + RTCP + activité utilisateur.

## Phases et état
- Phase 1: WebRTC-only vidéo: en place.
- Phase 1.1: transfert de fichiers via `FileData`: conservé.
- Phase 2: pipeline faible latence capture->encode->track: en place (capture abstraite + fallback).
- Phase 3: adaptation bitrate/FPS dynamique: en place (tiers + overrides + adaptation RTCP/CPU/activité).
- Phase 4: keyframes/reconnect robustes: en cours d’alignement final (policy reconnect stricte + arrêt sur non-retryables).
- Phase 5: refactor modulaire: partiellement en place (module `capture/`).
- Phase 6: logging structuré production: en cours (stats structurées étendues + cadence configurable).
- Phase 7: validation/rollout: à exécuter après stabilisation phases 4-6.

## Vérification cible
- `cd src-tauri && cargo check`
- `npm run -s check`
- Session nominale: Offer/Answer/ICE OK, track WebRTC actif, zéro vidéo via `FileData`.
- Réseau dégradé: transitions bitrate/FPS conformes.
- Leave explicite viewer: pas de reconnect.
- Panne réseau transitoire: reconnect + reprise.
- Close terminal `1003`: arrêt propre, pas de boucle retry.
- Fichiers: upload/download/list inchangés.
- Soak 30 min: stabilité latence/FPS/mémoire.

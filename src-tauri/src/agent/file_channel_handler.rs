//! File DataChannel handler — owns the agent-side logic for the
//! viewer-initiated "file" DataChannel:
//!  - browse remote directories (`FILE_LIST_REQUEST`)
//!  - send a file from the agent to the viewer (`FILE_DOWNLOAD_REQUEST`)
//!  - receive a file from the viewer (`FILE_UPLOAD_START` + binary chunks + `FILE_COMPLETE`)
//!  - serve agent-side screenshots requested by the AI pipeline
//!    (`request_screenshot` → chunked `screenshot_chunk_*` responses).
//!
//! Re-exported via `super::webrtc` so `AgentWebRtc::new` can wire them up when
//! the agent's `RTCPeerConnection` exposes the data channel.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::data_channel::RTCDataChannel;

use super::file_transfer::FileTransferService;

// ─── File DataChannel upload state ────────────────────────────────────────────

/// État d'un upload en cours côté agent.
///
/// On écrit en streaming dans un fichier temporaire `<dest>.part`, puis on
/// fait un rename atomique vers `<dest>` au FILE_COMPLETE. Ce design :
///   1. Évite d'accumuler des Go en RAM (gros transferts de VM disks, vidéos…).
///   2. Garantit qu'on n'expose JAMAIS un fichier final partiellement écrit :
///      le scanner antivirus (Defender, etc.) ne voit que le `.part`, puis
///      d'un coup le fichier final complet via le rename.
///   3. En cas de crash/session perdue mid-transfer, le `.part` reste
///      identifiable et est supprimé proactivement au prochain
///      FILE_UPLOAD_START sur le même nom.
struct FileChannelUploadState {
    transfer_id: String,
    /// Chemin final demandé (sans `.part`).
    dest_path: PathBuf,
    /// Chemin du fichier temporaire en cours d'écriture (`<dest>.part`).
    part_path: PathBuf,
    /// Handle ouvert vers le `.part`. Toujours fermé via `flush + sync_all`
    /// avant le rename.
    part_file: tokio::fs::File,
    received_chunks: usize,
    /// Octets écrits sur disque (used pour le plafond anti-OOM/anti-DOS).
    bytes_written: usize,
}


/// Capture l'ecran cote AGENT (pas via la frame WebRTC decodee qui peut etre
/// noire si emission_paused=true) puis renvoie sur le meme DataChannel :
///
///   `{"type":"screenshot_response","commandId":"...","data":"<base64>","width":W,"height":H}`
///
/// En cas d'echec capture (ex: pas d'ecran primaire trouvable, surface bloquee
/// par OBS, etc.), on renvoie `{"type":"screenshot_response","commandId":"...","error":"..."}`
/// pour que le viewer puisse afficher un message clair plutot que de timeout.
/// Limite de taille par message texte sur le DataChannel SCTP (webrtc-rs).
/// L'impl rejette tout > ~64 KB par message. On chunke a 14 KB pour avoir une
/// marge confortable (chunk + JSON envelope ~ 14.2 KB par send_text).
const SCREENSHOT_CHUNK_SIZE: usize = 14 * 1024;

/// Plafond sur l'upload pour eviter qu'un viewer malicieux/buggue ne fasse
/// exploser la memoire de l'agent en annonçant un totalChunks astronomique
/// (chaque chunk = 64 KB → 8192 chunks = 512 MB). On refuse au-delà.
const MAX_UPLOAD_BYTES: usize = 512 * 1024 * 1024;
const UPLOAD_CHUNK_SIZE: usize = 64 * 1024;
const MAX_UPLOAD_CHUNKS: usize = MAX_UPLOAD_BYTES / UPLOAD_CHUNK_SIZE;

pub(super) async fn handle_screenshot_request(channel: Arc<RTCDataChannel>, command_id: String) {
    tracing::info!("📸 [{command_id}] starting capture (1280px width, JPEG q=50)…");

    let result = super::screen_capture::capture_primary_jpeg_base64_scaled(1280, 50);

    match result {
        Ok((b64, w, h)) => {
            let size_kb = b64.len() / 1024;
            tracing::info!(
                "📸 [{command_id}] capture OK ({size_kb} KB base64, {w}x{h}) — chunking"
            );
            send_screenshot_chunked(&channel, &command_id, &b64, w, h).await;
        }
        Err(e) => {
            tracing::warn!("❌ [{command_id}] capture FAILED: {e}");
            // L'erreur tient en 1 message (petit) → pas de chunking.
            let err_msg = serde_json::json!({
                "type": "screenshot_response_error",
                "commandId": command_id,
                "error": format!("agent capture failed: {e}"),
            });
            if let Err(e) = channel.send_text(err_msg.to_string()).await {
                tracing::warn!("❌ [{command_id}] send_text error reply failed: {e}");
            }
        }
    }
}

/// Decoupe le base64 en chunks de SCREENSHOT_CHUNK_SIZE et les envoie sous
/// forme de 3 types de messages :
///   1. screenshot_chunk_start { commandId, totalChunks, totalBytes, width, height }
///   2. screenshot_chunk       { commandId, index, data }  (N fois)
///   3. screenshot_chunk_end   { commandId }
///
/// Le viewer reassemble dans handleScreenshotResponse() en concatenant les
/// chunks par index. L'ordre est garanti par ordered=true sur le DataChannel,
/// mais l'index permet quand meme de detecter une perte/incoherence.
async fn send_screenshot_chunked(
    channel: &Arc<RTCDataChannel>,
    command_id: &str,
    base64_data: &str,
    width: u32,
    height: u32,
) {
    let bytes = base64_data.as_bytes();
    let total_bytes = bytes.len();
    let total_chunks = total_bytes.div_ceil(SCREENSHOT_CHUNK_SIZE);

    // ─── Header ────────────────────────────────────────────────────────────
    let header = serde_json::json!({
        "type": "screenshot_chunk_start",
        "commandId": command_id,
        "totalChunks": total_chunks,
        "totalBytes": total_bytes,
        "width": width,
        "height": height,
    });
    if let Err(e) = channel.send_text(header.to_string()).await {
        tracing::warn!("❌ [{command_id}] chunk_start failed: {e}");
        return;
    }

    // ─── Chunks ────────────────────────────────────────────────────────────
    for (i, chunk) in bytes.chunks(SCREENSHOT_CHUNK_SIZE).enumerate() {
        // chunk est en bytes, mais base64 est ASCII donc safe a recompose.
        // Edge case : on a coupé en plein milieu d'un caractere UTF-8 ?
        // Non — base64 ne contient que [A-Za-z0-9+/=] (ASCII single-byte).
        let chunk_str = match std::str::from_utf8(chunk) {
            Ok(s) => s,
            Err(_) => {
                tracing::warn!("❌ [{command_id}] chunk {i} not valid UTF-8 (bug)");
                return;
            }
        };
        let chunk_msg = serde_json::json!({
            "type": "screenshot_chunk",
            "commandId": command_id,
            "index": i,
            "data": chunk_str,
        });
        if let Err(e) = channel.send_text(chunk_msg.to_string()).await {
            tracing::warn!("❌ [{command_id}] chunk {i}/{total_chunks} send failed: {e}");
            return;
        }
    }

    // ─── End marker ────────────────────────────────────────────────────────
    let end_msg = serde_json::json!({
        "type": "screenshot_chunk_end",
        "commandId": command_id,
    });
    if let Err(e) = channel.send_text(end_msg.to_string()).await {
        tracing::warn!("❌ [{command_id}] chunk_end failed: {e}");
        return;
    }

    tracing::info!(
        "✅ [{command_id}] sent {total_chunks} chunks, {} KB total ({} KB/chunk avg)",
        total_bytes / 1024,
        if total_chunks > 0 { (total_bytes / total_chunks) / 1024 } else { 0 }
    );
}

// ─── File DataChannel helpers ─────────────────────────────────────────────────

/// Configure the "file" DataChannel received from the viewer.
/// Handles JSON control messages and binary upload chunks.
pub(super) async fn setup_file_channel(channel: Arc<RTCDataChannel>, allow_file_transfer: bool) {
    let upload_state: Arc<Mutex<Option<FileChannelUploadState>>> =
        Arc::new(Mutex::new(None));
    let channel_for_msg = Arc::clone(&channel);

    channel.on_message(Box::new(move |msg: DataChannelMessage| {
        let channel = Arc::clone(&channel_for_msg);
        let upload_state = Arc::clone(&upload_state);
        Box::pin(async move {
            if msg.is_string {
                let Ok(text) = String::from_utf8(msg.data.to_vec()) else { return; };
                let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else { return; };

                if !allow_file_transfer {
                    let tid = json["transferId"].as_str().unwrap_or("").to_string();
                    let _ = channel.send_text(serde_json::json!({
                        "type": "FILE_ERROR",
                        "transferId": tid,
                        "message": "File transfer not permitted"
                    }).to_string()).await;
                    return;
                }

                match json["type"].as_str() {
                    Some("FILE_LIST_REQUEST") => {
                        let path = json["path"].as_str().unwrap_or("").to_string();
                        tracing::info!("📂 [file-ch] Listing: {path}");
                        let listing = FileTransferService::new().get_directory_listing(&path);
                        let resp = serde_json::json!({
                            "type": "FILE_LIST_RESPONSE",
                            "path": listing.path,
                            "files": listing.files,
                            "error": listing.error,
                        });
                        let _ = channel.send_text(resp.to_string()).await;
                    }
                    Some("FILE_DOWNLOAD_REQUEST") => {
                        let path = json["path"].as_str().unwrap_or("").to_string();
                        let tid = json["transferId"].as_str().unwrap_or("").to_string();
                        tracing::info!("📥 [file-ch] Download: {path}");
                        tokio::spawn(handle_file_download(Arc::clone(&channel), path, tid));
                    }
                    Some("FILE_UPLOAD_START") => {
                        let tid = json["transferId"].as_str().unwrap_or("").to_string();
                        let file_name = json["fileName"].as_str().unwrap_or("upload").to_string();
                        let total_chunks = json["totalChunks"].as_u64().unwrap_or(1);

                        // Anti-DOS : un viewer compromis ne doit pas pouvoir
                        // demander une allocation gigantesque en annoncant un
                        // totalChunks arbitraire. On rejette avant meme de
                        // commencer.
                        if (total_chunks as usize) > MAX_UPLOAD_CHUNKS {
                            tracing::warn!(
                                "⛔ [file-ch] Upload refuse tid={tid} totalChunks={total_chunks} > MAX ({MAX_UPLOAD_CHUNKS})"
                            );
                            let _ = channel.send_text(serde_json::json!({
                                "type": "FILE_ERROR",
                                "transferId": tid,
                                "message": format!(
                                    "Upload rejected: too large ({} chunks > {} max = {} MB)",
                                    total_chunks, MAX_UPLOAD_CHUNKS, MAX_UPLOAD_BYTES / (1024 * 1024)
                                ),
                            }).to_string()).await;
                            return;
                        }

                        let safe_name = std::path::Path::new(&file_name)
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| "upload".to_string());

                        // On sauve dans un sous-dossier qu'on POSSÈDE et nomme
                        // explicitement : `<Downloads>/LumiereTransfers/`.
                        // Avantages :
                        //  - Nom identique en anglais/français/etc → l'user le
                        //    trouve sans confusion sur Windows localisé
                        //  - On crée le dossier nous-mêmes → pas de surprise
                        //    si Downloads est introuvable
                        //  - Isolé du dossier Downloads natif → pas de conflit
                        //    avec d'autres fichiers
                        let base_downloads = FileTransferService::get_downloads_path();
                        let downloads_path = base_downloads.join("LumiereTransfers");
                        match tokio::fs::create_dir_all(&downloads_path).await {
                            Ok(_) => tracing::info!(
                                "📁 [file-ch] Sub-folder ready: {}",
                                downloads_path.display()
                            ),
                            Err(e) => tracing::warn!(
                                "❌ [file-ch] Cannot create sub-folder {}: {e}",
                                downloads_path.display()
                            ),
                        }

                        // Résolution canonique du chemin (suit symlinks/junctions)
                        let dest_buf = downloads_path.join(&safe_name);
                        let dest = dest_buf.to_string_lossy().to_string();

                        // Diagnostic absolu + canonicalisation pour résoudre
                        // les junctions / liens (sur Windows FR le dossier
                        // peut être affiché "Téléchargements" mais s'appelle
                        // bien "Downloads" sur disque, ou inversement).
                        let canonical = tokio::fs::canonicalize(&downloads_path).await.ok();
                        tracing::info!(
                            "🔍 [file-ch] AGENT host={:?} userprofile={:?} downloads_dir={} canonical={:?} target={}",
                            hostname::get().ok().map(|h| h.to_string_lossy().into_owned()),
                            std::env::var("USERPROFILE").ok(),
                            downloads_path.display(),
                            canonical.as_ref().map(|p| p.display().to_string()),
                            dest
                        );

                        // Liste le contenu actuel du dossier pour aider à
                        // comprendre où chercher si le user ne trouve pas
                        if let Ok(mut entries) = tokio::fs::read_dir(&downloads_path).await {
                            let mut names: Vec<String> = Vec::new();
                            while let Ok(Some(entry)) = entries.next_entry().await {
                                if let Some(n) = entry.file_name().to_str() {
                                    names.push(n.to_string());
                                }
                                if names.len() >= 20 { break; }
                            }
                            tracing::info!("🔍 [file-ch] {} contient (max 20): {:?}", downloads_path.display(), names);
                        } else {
                            tracing::warn!("⚠️ [file-ch] Cannot list {}", downloads_path.display());
                        }

                        // Nettoie tout reliquat : le fichier final ET le `.part`
                        // d'un upload précédent qui aurait été interrompu.
                        let part_buf = dest_buf.with_extension(
                            dest_buf
                                .extension()
                                .and_then(|e| e.to_str())
                                .map(|e| format!("{e}.part"))
                                .unwrap_or_else(|| "part".to_string()),
                        );
                        let _ = tokio::fs::remove_file(&dest_buf).await;
                        let _ = tokio::fs::remove_file(&part_buf).await;
                        tracing::info!(
                            "📤 [file-ch] Upload START tid={tid} file='{safe_name}' totalChunks={total_chunks} → {dest}"
                        );

                        // Ouvre le `.part` en écriture exclusive. Si l'open
                        // échoue (permission, disque plein, AV qui bloque),
                        // on remonte une erreur claire au viewer plutôt que
                        // d'accepter des chunks qu'on devra jeter ensuite.
                        let part_file = match tokio::fs::OpenOptions::new()
                            .write(true)
                            .create(true)
                            .truncate(true)
                            .open(&part_buf)
                            .await
                        {
                            Ok(f) => f,
                            Err(e) => {
                                tracing::warn!(
                                    "❌ [file-ch] Cannot open .part file {}: {e}",
                                    part_buf.display()
                                );
                                let _ = channel.send_text(serde_json::json!({
                                    "type": "FILE_ERROR",
                                    "transferId": tid,
                                    "message": format!("Cannot create temp file: {e}"),
                                }).to_string()).await;
                                return;
                            }
                        };

                        *upload_state.lock().await = Some(FileChannelUploadState {
                            transfer_id: tid.clone(),
                            dest_path: dest_buf.clone(),
                            part_path: part_buf,
                            part_file,
                            received_chunks: 0,
                            bytes_written: 0,
                        });

                        // Confirme au viewer qu'on a bien enregistré le start
                        // (utile pour debug — le viewer peut afficher la cible)
                        let _ = channel.send_text(serde_json::json!({
                            "type": "FILE_UPLOAD_STARTED",
                            "transferId": tid,
                            "destPath": dest,
                        }).to_string()).await;
                    }
                    Some("FILE_COMPLETE") => {
                        let tid = json["transferId"].as_str().unwrap_or("").to_string();
                        // `take()` plutôt que `clone()` : `tokio::fs::File` n'est
                        // pas Clone, et on veut de toute façon récupérer la
                        // propriété du writer pour le `flush + sync` final.
                        let owned = {
                            let mut state = upload_state.lock().await;
                            let matches = state.as_ref()
                                .map(|s| s.transfer_id == tid)
                                .unwrap_or(false);
                            if matches { state.take() } else { None }
                        };

                        let Some(mut snap) = owned else {
                            tracing::warn!(
                                "⚠️ [file-ch] FILE_COMPLETE tid={tid} mais pas d'upload_state — chunks perdus ?"
                            );
                            let _ = channel.send_text(serde_json::json!({
                                "type": "FILE_ERROR",
                                "transferId": tid,
                                "message": "No upload state on agent — FILE_UPLOAD_START never received or processed",
                            }).to_string()).await;
                            return;
                        };

                        let dest_buf = snap.dest_path.clone();
                        let part_buf = snap.part_path.clone();
                        let dest = dest_buf.to_string_lossy().to_string();
                        let received = snap.received_chunks;
                        let bytes_written = snap.bytes_written;

                        tracing::info!(
                            "💾 [file-ch] Finalizing tid={tid} chunks={received} bytes={bytes_written} → {dest}"
                        );

                        // Étape 1 — flush + sync sur le `.part`. Garantit que
                        // les octets sont effectivement sur disque AVANT le
                        // rename atomique (sinon Defender pourrait scanner un
                        // fichier vide et lever une false-positive panic).
                        let flush_result: Result<(), String> = async {
                            snap.part_file.flush().await
                                .map_err(|e| format!("flush: {e}"))?;
                            snap.part_file.sync_all().await
                                .map_err(|e| format!("sync: {e}"))?;
                            Ok(())
                        }.await;

                        if let Err(e) = flush_result {
                            tracing::warn!("❌ [file-ch] flush/sync failed for {}: {e}", part_buf.display());
                            let _ = tokio::fs::remove_file(&part_buf).await;
                            let _ = channel.send_text(serde_json::json!({
                                "type": "FILE_ERROR",
                                "transferId": tid,
                                "message": format!("Cannot finalize file at {dest}: {e}"),
                            }).to_string()).await;
                            return;
                        }
                        // Drop explicite : libère le handle Windows AVANT le
                        // rename (sinon "fichier utilisé par un autre process").
                        drop(snap.part_file);

                        // Étape 2 — rename atomique `.part` → final. Sur Windows
                        // ce n'est PAS atomique cross-volume mais ici on est
                        // dans le même dossier, donc OK. Si la cible existe
                        // déjà (race avec un autre upload), on l'écrase via
                        // remove + rename.
                        if let Err(e) = tokio::fs::rename(&part_buf, &dest_buf).await {
                            tracing::warn!(
                                "⚠️ [file-ch] rename {} → {} failed: {e}",
                                part_buf.display(), dest_buf.display()
                            );
                            // Retry après suppression du target (cas Windows
                            // où rename refuse d'écraser).
                            let _ = tokio::fs::remove_file(&dest_buf).await;
                            if let Err(e2) = tokio::fs::rename(&part_buf, &dest_buf).await {
                                tracing::warn!("❌ [file-ch] rename retry failed: {e2}");
                                let _ = tokio::fs::remove_file(&part_buf).await;
                                let _ = channel.send_text(serde_json::json!({
                                    "type": "FILE_ERROR",
                                    "transferId": tid,
                                    "message": format!("Cannot rename .part to final: {e2}"),
                                }).to_string()).await;
                                return;
                            }
                        }

                        // Étape 3 — vérification post-rename + ACK
                        let metadata = tokio::fs::metadata(&dest_buf).await;
                        let canonical = tokio::fs::canonicalize(&dest_buf).await
                            .map(|p| p.to_string_lossy().to_string())
                            .ok();
                        match metadata {
                            Ok(meta) => {
                                let final_path = canonical.clone().unwrap_or_else(|| dest.clone());
                                tracing::info!(
                                    "✅ [file-ch] Upload COMPLETE tid={tid} chunks={received} size={} → {final_path}",
                                    meta.len()
                                );
                                let _ = channel.send_text(serde_json::json!({
                                    "type": "FILE_UPLOAD_ACK",
                                    "transferId": tid,
                                    "destPath": dest,
                                    "canonicalPath": canonical,
                                    "size": meta.len(),
                                }).to_string()).await;
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "❌ [file-ch] {bytes_written} bytes written but metadata fails for {dest}: {e}"
                                );
                                let _ = channel.send_text(serde_json::json!({
                                    "type": "FILE_ERROR",
                                    "transferId": tid,
                                    "message": format!("File missing after rename at {dest}: {e}"),
                                }).to_string()).await;
                            }
                        }
                    }
                    _ => {}
                }
            } else {
                // Binary chunk → écrit directement dans le `.part` (streaming).
                // Plus de buffer Vec<u8> en RAM : un transfert de 500 MB
                // consomme désormais ~64 KB de pic, pas 500 MB.
                if !allow_file_transfer {
                    return;
                }
                let bytes_len = msg.data.len();

                let (write_err, abort_info, chunk_idx) = {
                    let mut state = upload_state.lock().await;
                    let Some(s) = state.as_mut() else {
                        tracing::warn!(
                            "⚠️ [file-ch] Binary chunk reçu sans upload_state actif (size={bytes_len}) — ignoré"
                        );
                        return;
                    };

                    // Anti-OOM/DOS : si totalChunks était mensonger ou si le
                    // viewer continue de pousser des chunks au-delà du plafond,
                    // on coupe la transmission.
                    if s.bytes_written.saturating_add(bytes_len) > MAX_UPLOAD_BYTES {
                        tracing::warn!(
                            "⛔ [file-ch] Upload tid={} abort: {} + {} > MAX_UPLOAD_BYTES",
                            s.transfer_id, s.bytes_written, bytes_len
                        );
                        let tid_err = s.transfer_id.clone();
                        let part_to_clean = s.part_path.clone();
                        *state = None;
                        drop(state);
                        // Nettoyage du `.part` orphelin
                        let _ = tokio::fs::remove_file(&part_to_clean).await;
                        let _ = channel.send_text(serde_json::json!({
                            "type": "FILE_ERROR",
                            "transferId": tid_err,
                            "message": format!(
                                "Upload aborted: exceeded max size {} MB",
                                MAX_UPLOAD_BYTES / (1024 * 1024)
                            ),
                        }).to_string()).await;
                        return;
                    }

                    // Écriture incrémentale dans le `.part`. Une erreur
                    // d'écriture (disque plein, AV qui locke) doit aborter
                    // proprement plutôt que silently dropper les chunks.
                    let write_res = s.part_file.write_all(&msg.data).await;
                    match write_res {
                        Ok(()) => {
                            s.bytes_written = s.bytes_written.saturating_add(bytes_len);
                            s.received_chunks += 1;
                            (None, None, s.received_chunks)
                        }
                        Err(e) => {
                            let tid_err = s.transfer_id.clone();
                            let part_to_clean = s.part_path.clone();
                            *state = None;
                            (Some(e.to_string()), Some((tid_err, part_to_clean)), 0)
                        }
                    }
                };

                if let (Some(err), Some((tid_err, part_to_clean))) = (write_err, abort_info) {
                    tracing::warn!("❌ [file-ch] write to .part failed: {err}");
                    let _ = tokio::fs::remove_file(&part_to_clean).await;
                    let _ = channel.send_text(serde_json::json!({
                        "type": "FILE_ERROR",
                        "transferId": tid_err,
                        "message": format!("Write to disk failed: {err}"),
                    }).to_string()).await;
                    return;
                }

                if chunk_idx == 1 || chunk_idx % 16 == 0 {
                    tracing::info!(
                        "📦 [file-ch] chunk #{chunk_idx} ({bytes_len} bytes) écrit sur disque"
                    );
                }
            }
        })
    }));
}

/// Send a file from the agent filesystem to the viewer over the DataChannel.
/// Called in a spawned task — blocks until the transfer is done or the channel
/// closes.
async fn handle_file_download(
    channel: Arc<RTCDataChannel>,
    path: String,
    transfer_id: String,
) {
    use tokio::io::AsyncReadExt;
    const CHUNK_SIZE: usize = 64 * 1024;       // 64 KB per chunk
    const MAX_BUFFERED: u64 = 4 * 1024 * 1024; // pause when > 4 MB buffered

    // Stat first so we can send totalSize / totalChunks up-front.
    let total_size = match tokio::fs::metadata(&path).await {
        Ok(m) => m.len(),
        Err(e) => {
            let _ = channel.send_text(serde_json::json!({
                "type": "FILE_ERROR",
                "transferId": transfer_id,
                "message": format!("Cannot access file: {e}"),
            }).to_string()).await;
            return;
        }
    };

    let total_chunks =
        ((total_size + CHUNK_SIZE as u64 - 1) / CHUNK_SIZE as u64).max(1) as usize;

    let file_name = std::path::Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.clone());

    // Send header
    let _ = channel.send_text(serde_json::json!({
        "type": "FILE_DOWNLOAD_RESPONSE",
        "transferId": transfer_id,
        "fileName": file_name,
        "totalSize": total_size,
        "totalChunks": total_chunks,
    }).to_string()).await;

    let mut file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(e) => {
            let _ = channel.send_text(serde_json::json!({
                "type": "FILE_ERROR",
                "transferId": transfer_id,
                "message": format!("Cannot open file: {e}"),
            }).to_string()).await;
            return;
        }
    };

    let mut buf = vec![0u8; CHUNK_SIZE];
    loop {
        // Backpressure: yield until the send buffer drains below threshold.
        while (channel.buffered_amount().await as u64) > MAX_BUFFERED {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        let n = match file.read(&mut buf).await {
            Ok(0) => break, // EOF
            Ok(n) => n,
            Err(e) => {
                let _ = channel.send_text(serde_json::json!({
                    "type": "FILE_ERROR",
                    "transferId": transfer_id,
                    "message": format!("Read error: {e}"),
                }).to_string()).await;
                return;
            }
        };

        if channel
            .send(&bytes::Bytes::copy_from_slice(&buf[..n]))
            .await
            .is_err()
        {
            return; // channel closed mid-transfer
        }
    }

    let _ = channel.send_text(serde_json::json!({
        "type": "FILE_COMPLETE",
        "transferId": transfer_id,
    }).to_string()).await;

    tracing::info!("✅ [file-ch] Download envoyé: {path}");
}


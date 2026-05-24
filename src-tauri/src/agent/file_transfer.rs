//! File transfer service.
//!
//! Port of `FileTransferService.cs` — replaces .NET `File`/`Directory`/`DriveInfo`
//! with `std::fs` (sync listing) and `tokio::fs` (async read/write).
//!
//! Protocol is identical to the C# version:
//!   - Directory listing  → `FileListResponse`  (JSON camelCase)
//!   - File download      → stream of `FileDataChunk` (base64, 64 KB each)
//!   - File upload        → save base64 chunks to Downloads folder

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const CHUNK_SIZE: usize = 64 * 1024; // 64 KB — same as C#

// ─── Data models ──────────────────────────────────────────────────────────────

/// Equivalent of `FileInfo` inner class in C#.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub is_directory: bool,
    pub size: u64,
    pub last_modified: i64, // Unix ms
}

/// Equivalent of `FileListResponse`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileListResponse {
    pub path: String,
    pub files: Vec<FileEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Equivalent of `FileDataChunk`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDataChunk {
    pub file_name: String,
    pub file_path: String,
    pub chunk_index: usize,
    pub total_chunks: usize,
    pub data: String,       // base64
    pub total_size: u64,
}

// ─── FileTransferService ──────────────────────────────────────────────────────

pub struct FileTransferService;

impl FileTransferService {
    pub fn new() -> Self { Self }

    // ── GetDirectoryListing ───────────────────────────────────────────────────
    /// Equivalent of `GetDirectoryListing(path)`.
    pub fn get_directory_listing(&self, path: &str) -> FileListResponse {
        let mut response = FileListResponse {
            path: path.to_string(),
            files: Vec::new(),
            error: None,
        };

        // Default to user home (same as C#: `Environment.SpecialFolder.UserProfile`)
        let resolved = if path.is_empty() || path == "/" {
            dirs_path_home()
        } else {
            // Windows: "C:" → "C:\" (same fix as C#)
            let p = if path.len() == 2 && path.ends_with(':') {
                format!("{}\\", path)
            } else {
                path.to_string()
            };
            PathBuf::from(p)
        };

        response.path = resolved.to_string_lossy().to_string();

        if !resolved.is_dir() {
            response.error = Some("Directory not found".into());
            return response;
        }

        // Parent entry ".."
        if let Some(parent) = resolved.parent() {
            response.files.push(FileEntry {
                name: "..".to_string(),
                path: parent.to_string_lossy().to_string(),
                is_directory: true,
                size: 0,
                last_modified: 0,
            });
        }

        // Root → list drives (Windows) or "/" (Unix)
        #[cfg(windows)]
        if resolved.parent().is_none() {
            for letter in b'A'..=b'Z' {
                let drive = format!("{}:\\", letter as char);
                if Path::new(&drive).exists() {
                    response.files.push(FileEntry {
                        name: drive.clone(),
                        path: drive,
                        is_directory: true,
                        size: 0,
                        last_modified: 0,
                    });
                }
            }
            return response;
        }

        // Subdirectories first
        match std::fs::read_dir(&resolved) {
            Err(e) => {
                response.error = Some(e.to_string());
                return response;
            }
            Ok(entries) => {
                // Plupart des dossiers Windows contiennent < 64 entrees ; on
                // pre-alloue raisonnablement pour eviter 6-7 reallocs sur les
                // dossiers chargés (Downloads, Bureau).
                let mut dirs: Vec<FileEntry> = Vec::with_capacity(32);
                let mut files: Vec<FileEntry> = Vec::with_capacity(32);

                for entry in entries.flatten() {
                    let meta = match entry.metadata() {
                        Ok(m) => m,
                        Err(_) => continue, // skip inaccessible (same as C# catch)
                    };
                    let last_modified = meta
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0);

                    let fe = FileEntry {
                        name: entry.file_name().to_string_lossy().to_string(),
                        path: entry.path().to_string_lossy().to_string(),
                        is_directory: meta.is_dir(),
                        size: if meta.is_file() { meta.len() } else { 0 },
                        last_modified,
                    };

                    if meta.is_dir() {
                        dirs.push(fe);
                    } else {
                        files.push(fe);
                    }
                }

                dirs.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
                files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

                response.files.extend(dirs);
                response.files.extend(files);
            }
        }

        response
    }

    // ── ReadFileChunks ────────────────────────────────────────────────────────
    /// Equivalent of `ReadFileChunks(filePath)` — yields base64 chunks.
    pub fn read_file_chunks(&self, file_path: &str) -> Vec<FileDataChunk> {
        let path = Path::new(file_path);
        let Ok(data) = std::fs::read(path) else {
            return Vec::new();
        };

        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        let total_size = data.len() as u64;
        let total_chunks = data.chunks(CHUNK_SIZE).count();

        data.chunks(CHUNK_SIZE)
            .enumerate()
            .map(|(i, chunk)| FileDataChunk {
                file_name: file_name.clone(),
                file_path: file_path.to_string(),
                chunk_index: i,
                total_chunks,
                data: B64.encode(chunk),
                total_size,
            })
            .collect()
    }

    // ── SaveFileAsync ─────────────────────────────────────────────────────────
    /// Equivalent of `SaveFileAsync(destinationPath, base64Data, append)`.
    pub async fn save_file_async(
        &self,
        destination_path: &str,
        base64_data: &str,
        append: bool,
    ) -> Result<(), String> {
        let bytes = B64.decode(base64_data).map_err(|e| e.to_string())?;

        let path = Path::new(destination_path);
        if let Some(dir) = path.parent() {
            tokio::fs::create_dir_all(dir)
                .await
                .map_err(|e| e.to_string())?;
        }

        use tokio::io::AsyncWriteExt;
        if append {
            let mut file = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .await
                .map_err(|e| e.to_string())?;
            file.write_all(&bytes).await.map_err(|e| e.to_string())?;
        } else {
            tokio::fs::write(path, &bytes)
                .await
                .map_err(|e| e.to_string())?;
        }

        Ok(())
    }

    // ── SaveFileBytes ─────────────────────────────────────────────────────────
    /// Write raw bytes to a file (no base64 decoding).  Used by the WebRTC
    /// DataChannel upload path which sends binary directly.
    pub async fn save_file_bytes(
        &self,
        destination_path: &str,
        data: &[u8],
        append: bool,
    ) -> Result<(), String> {
        let path = Path::new(destination_path);
        if let Some(dir) = path.parent() {
            tokio::fs::create_dir_all(dir)
                .await
                .map_err(|e| e.to_string())?;
        }

        use tokio::io::AsyncWriteExt;
        if append {
            let mut file = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .await
                .map_err(|e| e.to_string())?;
            file.write_all(data).await.map_err(|e| e.to_string())?;
        } else {
            tokio::fs::write(path, data)
                .await
                .map_err(|e| e.to_string())?;
        }

        Ok(())
    }

    // ── GetDownloadsPath ──────────────────────────────────────────────────────
    /// Equivalent of `GetDownloadsPath()` in `SessionManager.cs`.
    pub fn get_downloads_path() -> PathBuf {
        // 1) Sur Windows, on demande le chemin officiel via SHGetKnownFolderPath
        //    avec FOLDERID_Downloads. C'est la SEULE manière correcte de gérer
        //    les Windows localisés (où le dossier peut s'appeler "Téléchargements"
        //    sur disque), les dossiers redirigés (OneDrive), et les chemins
        //    personnalisés via Group Policy.
        #[cfg(windows)]
        if let Some(p) = downloads_via_shell_known_folder() {
            return p;
        }

        // 2) Fallback : recherche manuelle sous USERPROFILE / HOME
        let home = dirs_path_home();
        for candidate in &["Downloads", "Téléchargements", "Descargas", "下载", "Загрузки"] {
            let p = home.join(candidate);
            if p.exists() {
                return p;
            }
        }

        // 3) Dernière option : crée Downloads sous home
        home.join("Downloads")
    }
}

/// Résout le dossier Téléchargements via l'API Shell Windows officielle
/// (`SHGetKnownFolderPath` + `FOLDERID_Downloads`). Renvoie `None` si l'appel
/// échoue.
#[cfg(windows)]
fn downloads_via_shell_known_folder() -> Option<PathBuf> {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{
        FOLDERID_Downloads, SHGetKnownFolderPath, KF_FLAG_DEFAULT,
    };

    unsafe {
        let path_ptr = match SHGetKnownFolderPath(
            &FOLDERID_Downloads,
            KF_FLAG_DEFAULT,
            HANDLE::default(),
        ) {
            Ok(p) => p,
            Err(_) => return None,
        };
        if path_ptr.is_null() {
            return None;
        }
        // Mesure la longueur (NUL-terminated UTF-16)
        let mut len = 0usize;
        while *path_ptr.0.add(len) != 0 {
            len += 1;
        }
        let slice = std::slice::from_raw_parts(path_ptr.0, len);
        let s = String::from_utf16_lossy(slice);
        // Libère la mémoire allouée par SHGetKnownFolderPath
        CoTaskMemFree(Some(path_ptr.0 as *const _));
        Some(PathBuf::from(s))
    }
}

impl Default for FileTransferService {
    fn default() -> Self { Self::new() }
}

// ─── Helper ───────────────────────────────────────────────────────────────────

fn dirs_path_home() -> PathBuf {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

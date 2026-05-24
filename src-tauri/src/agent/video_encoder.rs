use std::env;
use std::net::UdpSocket as StdUdpSocket;
use std::process::Stdio;

use tokio::io::AsyncWriteExt;
use tokio::net::UdpSocket;
use tokio::process::{Child, ChildStdin, Command};
use tokio::time::{timeout, Duration};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoEncoderBackend {
    MediaFoundationH264,
    FfmpegNvenc,
    FfmpegQsv,
    FfmpegAmf,
    OpenH264Software,
}

#[derive(Clone, Copy, Debug)]
pub struct VideoEncoderPreset {
    pub target_fps: u32,
    pub bitrate_bps: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct VideoEncoderSelection {
    pub backend: VideoEncoderBackend,
    pub preset: VideoEncoderPreset,
}

impl VideoEncoderBackend {
    pub fn label(self) -> &'static str {
        match self {
            Self::MediaFoundationH264 => "native:media_foundation_h264",
            Self::FfmpegNvenc => "ffmpeg:h264_nvenc",
            Self::FfmpegQsv => "ffmpeg:h264_qsv",
            Self::FfmpegAmf => "ffmpeg:h264_amf",
            Self::OpenH264Software => "software:openh264",
        }
    }

    pub fn ffmpeg_encoder_name(self) -> Option<&'static str> {
        match self {
            Self::MediaFoundationH264 => None,
            Self::FfmpegNvenc => Some("h264_nvenc"),
            Self::FfmpegQsv => Some("h264_qsv"),
            Self::FfmpegAmf => Some("h264_amf"),
            Self::OpenH264Software => None,
        }
    }

    pub fn default_preset(self) -> VideoEncoderPreset {
        match self {
            Self::MediaFoundationH264 => VideoEncoderPreset {
                target_fps: 60,
                bitrate_bps: 8_000_000,
            },
            Self::FfmpegNvenc | Self::FfmpegQsv | Self::FfmpegAmf => VideoEncoderPreset {
                target_fps: 60,
                bitrate_bps: 8_000_000,
            },
            Self::OpenH264Software => VideoEncoderPreset {
                target_fps: 20,
                bitrate_bps: 3_000_000,
            },
        }
    }
}

impl VideoEncoderSelection {
    pub fn resolve() -> Self {
        let requested = env::var("LUMIERE_VIDEO_ENCODER").ok();
        let preferred = requested
            .as_deref()
            .map(parse_requested_backend)
            .unwrap_or(None);

        let backend = if let Some(backend) = preferred {
            match backend {
                VideoEncoderBackend::OpenH264Software => backend,
                VideoEncoderBackend::MediaFoundationH264 => backend,
                _ if ffmpeg_supports_backend(backend) => backend,
                _ => {
                    tracing::warn!(
                        "⚠️ Requested encoder backend '{}' not available. Falling back to auto.",
                        backend.label()
                    );
                    detect_best_backend()
                }
            }
        } else {
            detect_best_backend()
        };

        let default_preset = backend.default_preset();
        let target_fps = env::var("LUMIERE_TARGET_FPS")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .map(|value| value.clamp(5, 60))
            .unwrap_or(default_preset.target_fps);
        let bitrate_bps = env::var("LUMIERE_TARGET_BITRATE")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .map(|value| value.clamp(500_000, 20_000_000))
            .unwrap_or(default_preset.bitrate_bps);

        Self {
            backend,
            preset: VideoEncoderPreset {
                target_fps,
                bitrate_bps,
            },
        }
    }
}

pub struct FfmpegRtpBridge {
    child: Child,
    stdin: ChildStdin,
    socket: UdpSocket,
}

impl FfmpegRtpBridge {
    pub async fn spawn(
        backend: VideoEncoderBackend,
        width: usize,
        height: usize,
        preset: VideoEncoderPreset,
        payload_type: u8,
    ) -> Result<Self, String> {
        let encoder_name = backend
            .ffmpeg_encoder_name()
            .ok_or_else(|| "Selected backend is not an FFmpeg backend".to_string())?;
        let ffmpeg_binary = ffmpeg_binary();

        let std_socket = StdUdpSocket::bind("127.0.0.1:0")
            .map_err(|err| format!("bind udp socket failed: {err}"))?;
        std_socket
            .set_nonblocking(true)
            .map_err(|err| format!("set_nonblocking failed: {err}"))?;
        let port = std_socket
            .local_addr()
            .map_err(|err| format!("local_addr failed: {err}"))?
            .port();
        let socket =
            UdpSocket::from_std(std_socket).map_err(|err| format!("from_std udp failed: {err}"))?;

        let bitrate = preset.bitrate_bps.to_string();
        // Low-latency GOP target: one keyframe every ~2 seconds.
        let gop = (preset.target_fps.saturating_mul(2)).max(30).to_string();
        let mut command = Command::new(ffmpeg_binary);
        command
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-fflags")
            .arg("nobuffer")
            .arg("-f")
            .arg("rawvideo")
            .arg("-pix_fmt")
            .arg("bgra")
            .arg("-video_size")
            .arg(format!("{width}x{height}"))
            .arg("-framerate")
            .arg(preset.target_fps.to_string())
            .arg("-i")
            .arg("pipe:0")
            .arg("-an")
            .arg("-c:v")
            .arg(encoder_name)
            .args(ffmpeg_backend_args(backend))
            .arg("-b:v")
            .arg(&bitrate)
            .arg("-minrate")
            .arg(&bitrate)
            .arg("-maxrate")
            .arg(&bitrate)
            .arg("-bufsize")
            .arg((preset.bitrate_bps.saturating_mul(2)).to_string())
            .arg("-g")
            .arg(gop)
            .arg("-bf")
            .arg("0")
            .arg("-f")
            .arg("rtp")
            .arg("-payload_type")
            .arg(payload_type.to_string())
            .arg(format!("rtp://127.0.0.1:{port}?pkt_size=1200"))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let mut child = command
            .spawn()
            .map_err(|err| format!("spawn ffmpeg failed: {err}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "ffmpeg stdin unavailable".to_string())?;

        Ok(Self {
            child,
            stdin,
            socket,
        })
    }

    pub async fn write_frame(&mut self, bgra_frame: &[u8]) -> Result<(), String> {
        self.stdin
            .write_all(bgra_frame)
            .await
            .map_err(|err| format!("ffmpeg stdin write failed: {err}"))
    }

    pub async fn try_read_packet(&self, buffer: &mut [u8]) -> Result<Option<usize>, String> {
        match timeout(Duration::from_millis(2), self.socket.recv(buffer)).await {
            Ok(Ok(size)) => Ok(Some(size)),
            Ok(Err(err)) => Err(format!("udp recv failed: {err}")),
            Err(_) => Ok(None),
        }
    }

    pub async fn shutdown(&mut self) {
        let _ = self.stdin.shutdown().await;
        let _ = self.child.kill().await;
    }
}

fn parse_requested_backend(raw: &str) -> Option<VideoEncoderBackend> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "auto" => None,
        "mf" | "mediafoundation" | "media_foundation" | "native_h264" => {
            Some(VideoEncoderBackend::MediaFoundationH264)
        }
        "nvenc" | "h264_nvenc" => Some(VideoEncoderBackend::FfmpegNvenc),
        "qsv" | "h264_qsv" => Some(VideoEncoderBackend::FfmpegQsv),
        "amf" | "h264_amf" => Some(VideoEncoderBackend::FfmpegAmf),
        "software" | "openh264" => Some(VideoEncoderBackend::OpenH264Software),
        _ => None,
    }
}

fn detect_best_backend() -> VideoEncoderBackend {
    #[cfg(windows)]
    {
        // Détection Intel UHD bas de gamme.
        let adapter = detect_primary_adapter_name();
        if let Some(name) = adapter.as_deref() {
            if is_low_power_intel_gpu(name) {
                tracing::info!(
                    "🎬 Encoder: OpenH264 (software) — adapter '{name}' détecté comme Intel intégré bas de gamme (MediaFoundation H264 instable sur ce GPU)"
                );
                return VideoEncoderBackend::OpenH264Software;
            }
            tracing::debug!("🎬 Primary adapter: {name}");
        }


        if env::var("LUMIERE_ENABLE_MF_AUTO")
            .ok()
            .map(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false)
        {
            tracing::info!(
                "🎬 Encoder: MediaFoundation H264 (LUMIERE_ENABLE_MF_AUTO=on, adapter '{}')",
                adapter.as_deref().unwrap_or("<unknown>")
            );
            return VideoEncoderBackend::MediaFoundationH264;
        }
        tracing::info!(
            "🎬 Encoder: OpenH264 (software, default Windows) on adapter '{}'",
            adapter.as_deref().unwrap_or("<unknown>")
        );
        return VideoEncoderBackend::OpenH264Software;
    }

    #[cfg(not(windows))]
    [
        VideoEncoderBackend::FfmpegNvenc,
        VideoEncoderBackend::FfmpegQsv,
        VideoEncoderBackend::FfmpegAmf,
    ]
    .into_iter()
    .find(|backend| ffmpeg_supports_backend(*backend))
    .unwrap_or(VideoEncoderBackend::OpenH264Software)
}


#[cfg(windows)]
fn detect_primary_adapter_name() -> Option<String> {
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1};

    unsafe {
        let factory: IDXGIFactory1 = match CreateDXGIFactory1() {
            Ok(f) => f,
            Err(err) => {
                tracing::warn!("🎬 DXGI factory unavailable for encoder selection: {err}");
                return None;
            }
        };

        for adapter_index in 0..16u32 {
            let adapter: IDXGIAdapter1 = match factory.EnumAdapters1(adapter_index) {
                Ok(a) => a,
                Err(_) => break,
            };
            let desc = match adapter.GetDesc1() {
                Ok(d) => d,
                Err(_) => continue,
            };
            // Cherche le premier adaptateur ayant au moins un output
            // attaché au desktop — c'est lui qui pilote l'affichage.
            let mut has_attached_output = false;
            for output_index in 0..16u32 {
                let Ok(output) = adapter.EnumOutputs(output_index) else { break };
                let Ok(odesc) = output.GetDesc() else { continue };
                if odesc.AttachedToDesktop.as_bool() {
                    has_attached_output = true;
                    break;
                }
            }
            if !has_attached_output {
                continue;
            }

            // UTF-16 fixed-array → String (trim sur le premier null).
            let chars = &desc.Description;
            let end = chars.iter().position(|c| *c == 0).unwrap_or(chars.len());
            return Some(String::from_utf16_lossy(&chars[..end]).trim().to_string());
        }
        None
    }
}

#[cfg(not(windows))]
fn detect_primary_adapter_name() -> Option<String> {
    None
}


pub(super) fn is_low_power_intel_gpu(adapter_name: &str) -> bool {
    let lower = adapter_name.to_ascii_lowercase();
    if !lower.contains("intel") {
        return false;
    }
    // Familles intégrées historiquement problématiques pour MF H264 :
    lower.contains("uhd graphics")
        || lower.contains("hd graphics")
        || lower.contains("iris plus")
        || lower.contains("iris xe graphics")
        || lower.contains("iris(r) plus")
        || lower.contains("iris(r) xe graphics")
}

fn ffmpeg_binary() -> String {
    env::var("LUMIERE_FFMPEG_PATH").unwrap_or_else(|_| "ffmpeg".to_string())
}

fn ffmpeg_supports_backend(backend: VideoEncoderBackend) -> bool {
    let Some(encoder_name) = backend.ffmpeg_encoder_name() else {
        return false;
    };

    let output = std::process::Command::new(ffmpeg_binary())
        .arg("-hide_banner")
        .arg("-encoders")
        .output();

    let Ok(output) = output else {
        return false;
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.lines().any(|line| line.contains(encoder_name))
}

fn ffmpeg_backend_args(backend: VideoEncoderBackend) -> &'static [&'static str] {
    match backend {
        VideoEncoderBackend::MediaFoundationH264 => &[],
        VideoEncoderBackend::FfmpegNvenc => &["-preset", "p4", "-tune", "ll", "-rc", "cbr_ld_hq"],
        VideoEncoderBackend::FfmpegQsv => &["-preset", "veryfast"],
        VideoEncoderBackend::FfmpegAmf => &["-usage", "ultralowlatency", "-quality", "speed"],
        VideoEncoderBackend::OpenH264Software => &[],
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── is_low_power_intel_gpu ──────────────────────────────────────────────

    #[test]
    fn detects_intel_uhd_730() {
        assert!(is_low_power_intel_gpu("Intel(R) UHD Graphics 730"));
    }

    #[test]
    fn detects_intel_uhd_770() {
        assert!(is_low_power_intel_gpu("Intel(R) UHD Graphics 770"));
    }

    #[test]
    fn detects_intel_hd_graphics_legacy() {
        assert!(is_low_power_intel_gpu("Intel(R) HD Graphics 620"));
        assert!(is_low_power_intel_gpu("Intel(R) HD Graphics 530"));
    }

    #[test]
    fn detects_iris_xe_integrated() {
        assert!(is_low_power_intel_gpu("Intel(R) Iris(R) Xe Graphics"));
        assert!(is_low_power_intel_gpu("Intel Iris Xe Graphics"));
    }

    #[test]
    fn detects_iris_plus() {
        assert!(is_low_power_intel_gpu("Intel(R) Iris(R) Plus Graphics"));
    }

    #[test]
    fn case_insensitive() {
        assert!(is_low_power_intel_gpu("INTEL(R) UHD GRAPHICS 730"));
        assert!(is_low_power_intel_gpu("intel uhd graphics 730"));
    }

    // ── Cas négatifs : ne PAS classer comme bas de gamme ────────────────────

    #[test]
    fn does_not_match_nvidia() {
        assert!(!is_low_power_intel_gpu("NVIDIA GeForce GTX 1650"));
        assert!(!is_low_power_intel_gpu("NVIDIA GeForce RTX 4090"));
    }

    #[test]
    fn does_not_match_amd() {
        assert!(!is_low_power_intel_gpu("AMD Radeon RX 6800"));
        assert!(!is_low_power_intel_gpu("Radeon(TM) Graphics"));
    }

    #[test]
    fn does_not_match_intel_arc_discrete() {
        // Arc A-Series = discret, MF H264 fiable → ne pas dégrader.
        assert!(!is_low_power_intel_gpu("Intel(R) Arc(TM) A770 Graphics"));
        assert!(!is_low_power_intel_gpu("Intel Arc A750"));
    }

    #[test]
    fn does_not_match_empty_or_unknown() {
        assert!(!is_low_power_intel_gpu(""));
        assert!(!is_low_power_intel_gpu("Unknown GPU"));
    }

    // ── Backend label / preset cohérence ─────────────────────────────────────

    #[test]
    fn open_h264_has_software_label() {
        assert!(VideoEncoderBackend::OpenH264Software.label().contains("software"));
    }

    #[test]
    fn open_h264_preset_is_modest() {
        // Sur GPU intégré on s'attend à un preset modéré.
        let preset = VideoEncoderBackend::OpenH264Software.default_preset();
        assert!(preset.target_fps <= 30);
        assert!(preset.bitrate_bps <= 5_000_000);
    }

    #[test]
    fn ffmpeg_encoder_name_is_set_for_hardware_backends() {
        assert_eq!(VideoEncoderBackend::FfmpegNvenc.ffmpeg_encoder_name(), Some("h264_nvenc"));
        assert_eq!(VideoEncoderBackend::FfmpegQsv.ffmpeg_encoder_name(), Some("h264_qsv"));
        assert_eq!(VideoEncoderBackend::FfmpegAmf.ffmpeg_encoder_name(), Some("h264_amf"));
    }

    #[test]
    fn ffmpeg_encoder_name_is_none_for_native_backends() {
        assert_eq!(VideoEncoderBackend::MediaFoundationH264.ffmpeg_encoder_name(), None);
        assert_eq!(VideoEncoderBackend::OpenH264Software.ffmpeg_encoder_name(), None);
    }

    // ── parse_requested_backend ─────────────────────────────────────────────

    #[test]
    fn parse_backend_handles_aliases() {
        assert_eq!(parse_requested_backend("mf"), Some(VideoEncoderBackend::MediaFoundationH264));
        assert_eq!(parse_requested_backend("MediaFoundation"), Some(VideoEncoderBackend::MediaFoundationH264));
        assert_eq!(parse_requested_backend("nvenc"), Some(VideoEncoderBackend::FfmpegNvenc));
        assert_eq!(parse_requested_backend("openh264"), Some(VideoEncoderBackend::OpenH264Software));
        assert_eq!(parse_requested_backend("software"), Some(VideoEncoderBackend::OpenH264Software));
    }

    #[test]
    fn parse_backend_auto_means_unspecified() {
        assert_eq!(parse_requested_backend("auto"), None);
        assert_eq!(parse_requested_backend("AUTO"), None);
    }

    #[test]
    fn parse_backend_unknown_returns_none() {
        assert_eq!(parse_requested_backend("hevc"), None);
        assert_eq!(parse_requested_backend(""), None);
    }
}

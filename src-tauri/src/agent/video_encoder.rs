use std::env;
use std::net::UdpSocket as StdUdpSocket;
use std::process::Stdio;

use tokio::io::AsyncWriteExt;
use tokio::net::UdpSocket;
use tokio::process::{Child, ChildStdin, Command};
use tokio::time::{timeout, Duration};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoEncoderBackend {
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
            Self::FfmpegNvenc => "ffmpeg:h264_nvenc",
            Self::FfmpegQsv => "ffmpeg:h264_qsv",
            Self::FfmpegAmf => "ffmpeg:h264_amf",
            Self::OpenH264Software => "software:openh264",
        }
    }

    pub fn ffmpeg_encoder_name(self) -> Option<&'static str> {
        match self {
            Self::FfmpegNvenc => Some("h264_nvenc"),
            Self::FfmpegQsv => Some("h264_qsv"),
            Self::FfmpegAmf => Some("h264_amf"),
            Self::OpenH264Software => None,
        }
    }

    pub fn default_preset(self) -> VideoEncoderPreset {
        match self {
            Self::FfmpegNvenc | Self::FfmpegQsv | Self::FfmpegAmf => VideoEncoderPreset {
                target_fps: 60,
                bitrate_bps: 8_000_000,
            },
            Self::OpenH264Software => VideoEncoderPreset {
                target_fps: 12,
                bitrate_bps: 2_000_000,
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
                _ if ffmpeg_supports_backend(backend) => backend,
                _ => {
                    eprintln!(
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
            .arg("96")
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
        "nvenc" | "h264_nvenc" => Some(VideoEncoderBackend::FfmpegNvenc),
        "qsv" | "h264_qsv" => Some(VideoEncoderBackend::FfmpegQsv),
        "amf" | "h264_amf" => Some(VideoEncoderBackend::FfmpegAmf),
        "software" | "openh264" => Some(VideoEncoderBackend::OpenH264Software),
        _ => None,
    }
}

fn detect_best_backend() -> VideoEncoderBackend {
    [
        VideoEncoderBackend::FfmpegNvenc,
        VideoEncoderBackend::FfmpegQsv,
        VideoEncoderBackend::FfmpegAmf,
    ]
    .into_iter()
    .find(|backend| ffmpeg_supports_backend(*backend))
    .unwrap_or(VideoEncoderBackend::OpenH264Software)
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
        VideoEncoderBackend::FfmpegNvenc => &["-preset", "p4", "-tune", "ll", "-rc", "cbr_ld_hq"],
        VideoEncoderBackend::FfmpegQsv => &["-preset", "veryfast"],
        VideoEncoderBackend::FfmpegAmf => &["-usage", "ultralowlatency", "-quality", "speed"],
        VideoEncoderBackend::OpenH264Software => &[],
    }
}

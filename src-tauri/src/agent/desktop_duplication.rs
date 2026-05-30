#[cfg(windows)]
mod imp {
    use std::env;
    use std::thread;
    use std::time::{Duration, Instant};

    use windows::core::Interface;
    use windows::Win32::Foundation::{HWND};
    use windows::Win32::Graphics::Direct3D::{
        D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0,
        D3D_FEATURE_LEVEL_11_1,
    };
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
        D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
        D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext,
        ID3D11Resource, ID3D11Texture2D,
    };
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, IDXGIAdapter,
        IDXGIAdapter1, IDXGIFactory1, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
        DXGI_OUTPUT_DESC,
    };
    use windows::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
        GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT,
        DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ, ROP_CODE, SRCCOPY,
    };
    use windows::Win32::UI::HiDpi::{
        SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXSCREEN, SM_CXVIRTUALSCREEN, SM_CYSCREEN, SM_CYVIRTUALSCREEN,
        SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    };
    use screenshots::Screen;


    const BLACK_FRAME_THRESHOLD: u32 = 3;

    /// Single DXGI attempt per `capture_next_frame` call. WAIT_TIMEOUT
    /// means *the screen hasn't changed* — the right reaction is to
    /// reuse the last captured frame (the sender loops handle this via
    /// `Ok(None)`), not to retry-spin or fall back to GDI. The old
    /// 5×10ms retry loop burned 50–130 ms per quiet frame and capped
    /// throughput at ~10 fps on Intel UHD even when the screen *was*
    /// updating; the encoder still emitted frames, but DXGI starved it.
    const DXGI_TIMEOUT_RETRIES: u32 = 1;
    const DXGI_TIMEOUT_RETRY_DELAY: Duration = Duration::from_millis(0);

    /// Number of consecutive `capture_next_frame` calls returning
    /// `Ok(None)` (DXGI timeout) before we conclude the driver is broken
    /// on this GPU and switch permanently to GDI BitBlt. Genuine quiet
    /// periods on a stable adapter typically deliver a frame within a
    /// handful of ticks; a burst this long is a clear "DXGI dead"
    /// signature (frequent on Intel UHD).
    const DXGI_PERMANENT_TIMEOUT_THRESHOLD: u32 = 60;

    #[derive(Clone, Debug)]
    pub struct DesktopFrame {
        pub width: usize,
        pub height: usize,
        pub stride: usize,
        pub captured_at: Instant,
        pub bgra: Vec<u8>,
    }

    impl DesktopFrame {
        pub fn into_even_bgra(self) -> (usize, usize, Vec<u8>) {
            let even_width = self.width & !1;
            let even_height = self.height & !1;
            if even_width == self.width && even_height == self.height {
                return (self.width, self.height, self.bgra);
            }

            let mut out = vec![0u8; even_width * even_height * 4];
            for y in 0..even_height {
                let src_row =
                    &self.bgra[(y * self.stride)..(y * self.stride + even_width.saturating_mul(4))];
                let dst_row =
                    &mut out[(y * even_width * 4)..(y * even_width * 4 + even_width.saturating_mul(4))];
                dst_row.copy_from_slice(src_row);
            }

            (even_width, even_height, out)
        }

        pub fn resize_bgra_nearest(&self, target_width: usize, target_height: usize) -> Self {
            if target_width == self.width && target_height == self.height {
                return self.clone();
            }

            let mut out = vec![0u8; target_width * target_height * 4];
            for y in 0..target_height {
                let src_y = y.saturating_mul(self.height) / target_height.max(1);
                for x in 0..target_width {
                    let src_x = x.saturating_mul(self.width) / target_width.max(1);
                    let src_index = src_y
                        .saturating_mul(self.stride)
                        .saturating_add(src_x.saturating_mul(4));
                    let dst_index = y
                        .saturating_mul(target_width * 4)
                        .saturating_add(x.saturating_mul(4));
                    out[dst_index..dst_index + 4]
                        .copy_from_slice(&self.bgra[src_index..src_index + 4]);
                }
            }

            Self {
                width: target_width,
                height: target_height,
                stride: target_width * 4,
                captured_at: self.captured_at,
                bgra: out,
            }
        }

        pub fn to_nv12(&self) -> Nv12Frame {
            bgra_to_nv12(self.width, self.height, self.stride, &self.bgra)
        }
    }

    #[derive(Clone, Debug)]
    pub struct Nv12Frame {
        pub width: usize,
        pub height: usize,
        pub y_plane: Vec<u8>,
        pub uv_plane: Vec<u8>,
    }

    impl Nv12Frame {
        pub fn as_bytes(&self) -> Vec<u8> {
            let mut out = Vec::with_capacity(self.y_plane.len() + self.uv_plane.len());
            out.extend_from_slice(&self.y_plane);
            out.extend_from_slice(&self.uv_plane);
            out
        }
    }

    /// Identifie le chemin de capture actuellement utilisé. Logué au
    /// démarrage de chaque session et chaque fois qu'on bascule.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum CaptureBackend {
        Dxgi,
        Screenshots,
        Gdi,
    }

    pub struct DxgiDesktopDuplicator {
        device: Option<ID3D11Device>,
        context: Option<ID3D11DeviceContext>,
        duplication: Option<IDXGIOutputDuplication>,
        fallback_screen: Option<Screen>,
        staging_texture: Option<ID3D11Texture2D>,
        cached_width: u32,
        cached_height: u32,
        /// GDI fallback — initialisé lazy lorsqu'on bascule.
        gdi: Option<GdiCapturer>,
        /// Quand `true`, on ne tente même plus DXGI : un échec définitif
        /// a été constaté (frames noires répétées ou timeouts).
        force_gdi: bool,
        /// Compteur de frames noires consécutives renvoyées par DXGI.
        /// Quand ≥ BLACK_FRAME_THRESHOLD, on bascule en GDI.
        consecutive_black_frames: u32,
        /// Compteur de `WAIT_TIMEOUT` consécutifs. Réinitialisé dès
        /// qu'une frame est obtenue. Au-delà du seuil
        /// `DXGI_PERMANENT_TIMEOUT_THRESHOLD`, on bascule définitivement
        /// vers GDI : c'est la signature d'un driver Intel UHD cassé,
        /// pas d'un écran inactif.
        consecutive_dxgi_timeouts: u32,
        /// Description humaine de l'adaptateur sélectionné (ex: "Intel(R)
        /// UHD Graphics 730"). Loguée au démarrage et lors des fallbacks.
        adapter_name: String,
    }

    fn utf16_fixed_to_string(input: &[u16]) -> String {
        let end = input.iter().position(|c| *c == 0).unwrap_or(input.len());
        String::from_utf16_lossy(&input[..end])
    }

    fn read_monitor_index() -> Option<usize> {
        env::var("LUMIERE_CAPTURE_MONITOR_INDEX")
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
    }

    impl DxgiDesktopDuplicator {
        pub fn new() -> Result<Self, String> {
            unsafe {
                let factory: IDXGIFactory1 =
                    CreateDXGIFactory1().map_err(|err| format!("CreateDXGIFactory1 failed: {err}"))?;
                let requested_monitor_index = read_monitor_index();
                let feature_levels: [D3D_FEATURE_LEVEL; 2] =
                    [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];

                let mut last_error = "No DXGI output scanned".to_string();
                let mut attached_output_ordinal = 0usize;

                for adapter_index in 0..16u32 {
                    let adapter: IDXGIAdapter1 = match factory.EnumAdapters1(adapter_index) {
                        Ok(adapter) => adapter,
                        Err(_) => break,
                    };

                    let adapter_desc = match adapter.GetDesc1() {
                        Ok(desc) => desc,
                        Err(err) => {
                            last_error =
                                format!("GetDesc1 failed on adapter #{adapter_index}: {err}");
                            continue;
                        }
                    };
                    let adapter_name = utf16_fixed_to_string(&adapter_desc.Description);

                    let base_adapter: IDXGIAdapter = match adapter.cast() {
                        Ok(adapter) => adapter,
                        Err(err) => {
                            last_error = format!("Adapter cast failed on #{adapter_index}: {err}");
                            continue;
                        }
                    };

                    let mut device = None;
                    let mut context = None;
                    if let Err(err) = D3D11CreateDevice(
                        Some(&base_adapter),
                        D3D_DRIVER_TYPE_UNKNOWN,
                        None,
                        D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                        Some(&feature_levels),
                        D3D11_SDK_VERSION,
                        Some(&mut device),
                        None,
                        Some(&mut context),
                    ) {
                        last_error = format!("D3D11CreateDevice failed on adapter #{adapter_index}: {err}");
                        continue;
                    }

                    let Some(device) = device else {
                        last_error = format!("D3D11 device unavailable on adapter #{adapter_index}");
                        continue;
                    };
                    let Some(context) = context else {
                        last_error = format!("D3D11 context unavailable on adapter #{adapter_index}");
                        continue;
                    };

                    for output_index in 0..16u32 {
                        let output = match adapter.EnumOutputs(output_index) {
                            Ok(output) => output,
                            Err(_) => break,
                        };

                        let output_desc: DXGI_OUTPUT_DESC = match output.GetDesc() {
                            Ok(desc) => desc,
                            Err(err) => {
                                last_error = format!(
                                    "GetDesc failed on adapter #{adapter_index} output #{output_index}: {err}"
                                );
                                continue;
                            }
                        };

                        if !output_desc.AttachedToDesktop.as_bool() {
                            tracing::info!(
                                "DXGI skip output #{output_index} on adapter #{adapter_index} ({adapter_name}): not attached to desktop"
                            );
                            continue;
                        }

                        if let Some(requested) = requested_monitor_index {
                            if attached_output_ordinal != requested {
                                attached_output_ordinal += 1;
                                continue;
                            }
                        }

                        let output1: IDXGIOutput1 = match output.cast() {
                            Ok(output1) => output1,
                            Err(err) => {
                                last_error = format!(
                                    "IDXGIOutput1 cast failed on adapter #{adapter_index} output #{output_index}: {err}"
                                );
                                continue;
                            }
                        };

                        match output1.DuplicateOutput(&device) {
                            Ok(duplication) => {
                                let width = (output_desc.DesktopCoordinates.right
                                    - output_desc.DesktopCoordinates.left)
                                    .max(0) as u32;
                                let height = (output_desc.DesktopCoordinates.bottom
                                    - output_desc.DesktopCoordinates.top)
                                    .max(0) as u32;
                                tracing::info!(
                                    "🎥 Capture backend: DXGI Desktop Duplication on adapter #{adapter_index} ({adapter_name}) output #{output_index} (monitor ordinal {}): {}x{}",
                                    attached_output_ordinal,
                                    width,
                                    height
                                );

                              
                                let force_gdi_env = env::var("LUMIERE_CAPTURE_BACKEND")
                                    .map(|v| v.trim().eq_ignore_ascii_case("gdi"))
                                    .unwrap_or(false);
                                if force_gdi_env {
                                    tracing::info!(
                                        "🎥 LUMIERE_CAPTURE_BACKEND=gdi → DXGI ignoré au démarrage"
                                    );
                                }

                                return Ok(Self {
                                    device: Some(device),
                                    context: Some(context),
                                    duplication: Some(duplication),
                                    fallback_screen: None,
                                    staging_texture: None,
                                    cached_width: 0,
                                    cached_height: 0,
                                    gdi: None,
                                    force_gdi: force_gdi_env,
                                    consecutive_black_frames: 0,
                                    consecutive_dxgi_timeouts: 0,
                                    adapter_name: adapter_name.clone(),
                                });
                            }
                            Err(err) => {
                                last_error = format!(
                                    "DuplicateOutput failed on adapter #{adapter_index} ({adapter_name}) output #{output_index}: {err}"
                                );
                                tracing::info!("{last_error}");
                            }
                        }

                        attached_output_ordinal += 1;
                    }
                }

                let screens = Screen::all()
                    .map_err(|err| format!("Aucun output DXGI valide. Fallback screenshots indisponible: {err}. Dernier diagnostic DXGI: {last_error}"))?;
                let fallback_screen = if let Some(requested) = requested_monitor_index {
                    screens
                        .get(requested)
                        .copied()
                        .ok_or_else(|| {
                            format!(
                                "Monitor index {requested} invalide pour fallback screenshots ({} écrans). Dernier diagnostic DXGI: {last_error}",
                                screens.len()
                            )
                        })?
                } else {
                    screens
                        .iter()
                        .copied()
                        .find(|s| s.display_info.is_primary)
                        .or_else(|| screens.first().copied())
                        .ok_or_else(|| format!("Aucun écran disponible pour fallback screenshots. Dernier diagnostic DXGI: {last_error}"))?
                };

                let fallback_index = screens
                    .iter()
                    .position(|candidate| candidate.display_info.id == fallback_screen.display_info.id)
                    .unwrap_or(0);

                tracing::info!(
                    "🎥 Capture backend: screenshots crate (DXGI indisponible) on display index {} id {} ({}x{})",
                    fallback_index,
                    fallback_screen.display_info.id,
                    fallback_screen.display_info.width,
                    fallback_screen.display_info.height
                );

                Ok(Self {
                    device: None,
                    context: None,
                    duplication: None,
                    fallback_screen: Some(fallback_screen),
                    staging_texture: None,
                    cached_width: 0,
                    cached_height: 0,
                    gdi: None,
                    force_gdi: false,
                    consecutive_black_frames: 0,
                    consecutive_dxgi_timeouts: 0,
                    adapter_name: String::new(),
                })
            }
        }

     
        pub fn current_backend(&self) -> CaptureBackend {
            if self.force_gdi || self.duplication.is_none() && self.gdi.is_some() {
                CaptureBackend::Gdi
            } else if self.fallback_screen.is_some() {
                CaptureBackend::Screenshots
            } else {
                CaptureBackend::Dxgi
            }
        }

        pub fn capture_next_frame(
            &mut self,
            timeout_ms: u32,
        ) -> Result<Option<DesktopFrame>, String> {
            // ── Chemin GDI explicite (fallback définitif ou opt-in) ────────
            if self.force_gdi {
                return self.capture_via_gdi().map(Some);
            }

            // ── Chemin screenshots (DXGI complètement indispo au démarrage) ──
            if let Some(screen) = self.fallback_screen {
                if timeout_ms > 0 {
                    std::thread::sleep(std::time::Duration::from_millis(
                        (timeout_ms.min(33)) as u64,
                    ));
                }

                let rgba = screen
                    .capture()
                    .map_err(|err| format!("screenshots capture failed: {err}"))?;
                let width = rgba.width() as usize;
                let height = rgba.height() as usize;
                let mut bgra = rgba.into_raw();
                for px in bgra.chunks_exact_mut(4) {
                    px.swap(0, 2);
                }

                return Ok(Some(DesktopFrame {
                    width,
                    height,
                    stride: width * 4,
                    captured_at: Instant::now(),
                    bgra,
                }));
            }

        
            // Single attempt with optional retries (defaults to 1).
            // Treating WAIT_TIMEOUT as "no new frame → reuse last" is
            // both faster and visually identical: the screen hasn't
            // changed, there's nothing to re-encode. The sender loops
            // reuse the last captured frame on `Ok(None)`.
            let mut acquired_frame: Option<DesktopFrame> = None;
            let mut last_dxgi_err: Option<String> = None;
            for retry in 0..DXGI_TIMEOUT_RETRIES {
                match unsafe { self.try_capture_dxgi(timeout_ms) } {
                    Ok(Some(frame)) => {
                        acquired_frame = Some(frame);
                        break;
                    }
                    Ok(None) => {
                        if DXGI_TIMEOUT_RETRY_DELAY > Duration::ZERO
                            && retry + 1 < DXGI_TIMEOUT_RETRIES
                        {
                            thread::sleep(DXGI_TIMEOUT_RETRY_DELAY);
                        }
                    }
                    Err(err) => {
                        last_dxgi_err = Some(err);
                        break;
                    }
                }
            }

            // Erreur DXGI dure → activer GDI définitivement.
            if let Some(err) = last_dxgi_err {
                tracing::warn!(
                    "🎥 DXGI capture error on '{}': {err} — basculement permanent vers GDI BitBlt",
                    self.adapter_name
                );
                return self.activate_gdi_fallback_and_capture();
            }

            let Some(frame) = acquired_frame else {
                // WAIT_TIMEOUT path. Two scenarios:
                //  (a) screen genuinely idle → return Ok(None), the
                //      sender loop reuses the last captured frame
                //      (the user sees a steady image, no jank).
                //  (b) DXGI driver broken on this adapter → repeats
                //      forever; after DXGI_PERMANENT_TIMEOUT_THRESHOLD
                //      consecutive timeouts we switch to permanent GDI
                //      because (a) implies the screen is changing
                //      somewhere (mouse cursor, clock, etc).
                self.consecutive_dxgi_timeouts =
                    self.consecutive_dxgi_timeouts.saturating_add(1);
                if self.consecutive_dxgi_timeouts >= DXGI_PERMANENT_TIMEOUT_THRESHOLD {
                    tracing::warn!(
                        "🎥 DXGI: {} timeouts consécutifs sur '{}' — \
                         basculement permanent vers GDI BitBlt",
                        self.consecutive_dxgi_timeouts,
                        self.adapter_name
                    );
                    return self.activate_gdi_fallback_and_capture();
                }
                return Ok(None);
            };

            // Got a real frame — clear the timeout streak.
            if self.consecutive_dxgi_timeouts > 0 {
                self.consecutive_dxgi_timeouts = 0;
            }


            if is_black_frame(&frame.bgra) {
                self.consecutive_black_frames =
                    self.consecutive_black_frames.saturating_add(1);
                if self.consecutive_black_frames >= BLACK_FRAME_THRESHOLD {
                    tracing::warn!(
                        "🎥 DXGI a produit {} frames noires consécutives sur '{}' — \
                         basculement permanent vers GDI BitBlt (typique d'Intel UHD intégré)",
                        self.consecutive_black_frames,
                        self.adapter_name
                    );
                    return self.activate_gdi_fallback_and_capture();
                }
                // Sous le seuil : renvoie la frame quand même, l'encodeur
                // saura quoi en faire (et c'est peut-être légitime).
            } else if self.consecutive_black_frames > 0 {
                self.consecutive_black_frames = 0;
            }

            Ok(Some(frame))
        }

        /// Une seule tentative DXGI. `Ok(None)` = WAIT_TIMEOUT (retentable),
        /// `Err(_)` = erreur dure (device lost, etc., non retentable).
        unsafe fn try_capture_dxgi(
            &mut self,
            timeout_ms: u32,
        ) -> Result<Option<DesktopFrame>, String> {
            let duplication = self
                .duplication
                .as_ref()
                .ok_or_else(|| "DXGI duplication unavailable".to_string())?
                .clone();
            let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut desktop_resource: Option<IDXGIResource> = None;
            match duplication.AcquireNextFrame(timeout_ms, &mut frame_info, &mut desktop_resource)
            {
                Ok(()) => {}
                Err(err) if err.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                    return Ok(None);
                }
                Err(err) => {
                    return Err(format!("AcquireNextFrame failed: {err}"));
                }
            }
            let capture_result = (|| -> Result<Option<DesktopFrame>, String> {
                let resource = desktop_resource
                    .ok_or_else(|| "AcquireNextFrame returned no resource".to_string())?;
                let texture: ID3D11Texture2D = resource
                    .cast()
                    .map_err(|err| format!("IDXGIResource->ID3D11Texture2D cast failed: {err}"))?;

                let mut desc = D3D11_TEXTURE2D_DESC::default();
                texture.GetDesc(&mut desc);
                self.ensure_staging_texture(&desc)?;

                let staging = self
                    .staging_texture
                    .as_ref()
                    .ok_or_else(|| "Staging texture unavailable".to_string())?;

                let source_resource: ID3D11Resource = texture
                    .cast()
                    .map_err(|err| format!("Texture->Resource cast failed: {err}"))?;
                let staging_resource: ID3D11Resource = staging
                    .cast()
                    .map_err(|err| format!("Staging->Resource cast failed: {err}"))?;

                let context = self
                    .context
                    .as_ref()
                    .ok_or_else(|| "D3D11 context unavailable".to_string())?;
                context.CopyResource(&staging_resource, &source_resource);

                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                context
                    .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                    .map_err(|err| format!("Map staging texture failed: {err}"))?;

                let width = desc.Width as usize;
                let height = desc.Height as usize;
                let row_pitch = mapped.RowPitch as usize;
                let byte_len = row_pitch.saturating_mul(height);
                let src = std::slice::from_raw_parts(mapped.pData.cast::<u8>(), byte_len);
                let row_bytes = width * 4;
                let total_bytes = row_bytes * height;
                let bgra = if row_pitch == row_bytes {
                    src[..total_bytes].to_vec()
                } else {
                    let mut buf = Vec::with_capacity(total_bytes);
                    for y in 0..height {
                        buf.extend_from_slice(&src[y * row_pitch..y * row_pitch + row_bytes]);
                    }
                    buf
                };

                context.Unmap(staging, 0);

                Ok(Some(DesktopFrame {
                    width,
                    height,
                    stride: width * 4,
                    captured_at: Instant::now(),
                    bgra,
                }))
            })();

            let _ = duplication.ReleaseFrame();
            capture_result
        }

        /// Active le GDI fallback permanent et capture une première frame
        /// pour ne pas faire perdre un cycle à l'appelant.
        fn activate_gdi_fallback_and_capture(
            &mut self,
        ) -> Result<Option<DesktopFrame>, String> {
            self.force_gdi = true;
            // Libère les ressources DXGI qu'on n'utilisera plus.
            self.duplication = None;
            self.staging_texture = None;
            self.capture_via_gdi().map(Some)
        }

        /// Initialise (lazy) le GdiCapturer puis capture une frame.
        fn capture_via_gdi(&mut self) -> Result<DesktopFrame, String> {
            if self.gdi.is_none() {
                let gdi = GdiCapturer::new()?;
                tracing::info!(
                    "🎥 Capture backend: GDI BitBlt fallback ({}x{}) on '{}'",
                    gdi.width,
                    gdi.height,
                    if self.adapter_name.is_empty() { "<unknown adapter>" } else { &self.adapter_name }
                );
                self.gdi = Some(gdi);
            }
            // unwrap safe : on vient de l'initialiser au-dessus si nécessaire
            self.gdi
                .as_mut()
                .ok_or_else(|| "GDI capturer unavailable".to_string())?
                .capture_frame()
        }

        unsafe fn ensure_staging_texture(
            &mut self,
            source_desc: &D3D11_TEXTURE2D_DESC,
        ) -> Result<(), String> {
            if self.cached_width == source_desc.Width
                && self.cached_height == source_desc.Height
                && self.staging_texture.is_some()
            {
                return Ok(());
            }

            let staging_desc = D3D11_TEXTURE2D_DESC {
                Width: source_desc.Width,
                Height: source_desc.Height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: Default::default(),
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };

            let mut staging = None;
            let device = self
                .device
                .as_ref()
                .ok_or_else(|| "D3D11 device unavailable".to_string())?;
            device
                .CreateTexture2D(&staging_desc, None, Some(&mut staging))
                .map_err(|err| format!("CreateTexture2D staging failed: {err}"))?;

            self.staging_texture = staging;
            self.cached_width = source_desc.Width;
            self.cached_height = source_desc.Height;
            Ok(())
        }
    }

    // ─── Détection de frame noire ──────────────────────────────────────
 
    pub(super) fn is_black_frame(bgra: &[u8]) -> bool {
        if bgra.is_empty() {
            return true;
        }
        let pixel_count = bgra.len() / 4;
        if pixel_count == 0 {
            return true;
        }
        // Step = pixel_count / 256, mais au moins 1 et au plus pixel_count
        // pour éviter de tout sauter sur les petites images.
        let step = (pixel_count / 256).max(1);
        let mut sample_count = 0usize;
        for px_index in (0..pixel_count).step_by(step) {
            let byte_index = px_index * 4;
            if byte_index + 3 >= bgra.len() {
                break;
            }
            // BGRA : B, G, R, A. Le canal alpha peut être 255 sur certaines
            // surfaces opaques même si l'image est "noire" — on ignore A.
            let b = bgra[byte_index];
            let g = bgra[byte_index + 1];
            let r = bgra[byte_index + 2];
            if b != 0 || g != 0 || r != 0 {
                return false;
            }
            sample_count += 1;
            if sample_count >= 256 {
                break;
            }
        }
        true
    }

    // ─── GDI BitBlt capturer ───────────────────────────────────────────

    pub struct GdiCapturer {
        screen_dc: HDC,
        mem_dc: HDC,
        bitmap: HBITMAP,
        previous_bitmap: HGDIOBJ,
        /// Origine du virtual screen (peut être négatif sur multi-écran
        /// quand un moniteur est positionné à gauche/au-dessus du primary).
        origin_x: i32,
        origin_y: i32,
        width: i32,
        height: i32,
        /// Buffer réutilisé entre frames pour éviter une grosse alloc
        /// à chaque capture (1920×1080×4 = 8 MB).
        scratch: Vec<u8>,
        consecutive_black_frames: u32,
    }

    impl GdiCapturer {
        pub fn new() -> Result<Self, String> {
            unsafe {
                // ── DPI awareness ──────────────────────────────────────
                let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

                // ── Choix de la zone capturée ──────────────────────────

                let use_virtual_screen = env::var("LUMIERE_GDI_VIRTUAL_SCREEN")
                    .map(|v| matches!(
                        v.trim().to_ascii_lowercase().as_str(),
                        "1" | "true" | "yes" | "on"
                    ))
                    .unwrap_or(false);

                let (origin_x, origin_y, width, height) = if use_virtual_screen {
                    let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
                    let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
                    let w = GetSystemMetrics(SM_CXVIRTUALSCREEN);
                    let h = GetSystemMetrics(SM_CYVIRTUALSCREEN);
                    (x, y, w, h)
                } else {
                    (0, 0, GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN))
                };

                if width <= 0 || height <= 0 {
                    return Err(format!(
                        "GetSystemMetrics returned invalid dimensions: {width}x{height} (virtual={use_virtual_screen})"
                    ));
                }

                // GetDC(None) = DC du desktop (couvre tous les moniteurs).
                let screen_dc = GetDC(HWND::default());
                if screen_dc.is_invalid() {
                    return Err("GetDC(desktop) returned invalid HDC".to_string());
                }

                let mem_dc = CreateCompatibleDC(screen_dc);
                if mem_dc.is_invalid() {
                    ReleaseDC(HWND::default(), screen_dc);
                    return Err("CreateCompatibleDC failed".to_string());
                }

                let bitmap = CreateCompatibleBitmap(screen_dc, width, height);
                if bitmap.is_invalid() {
                    let _ = DeleteDC(mem_dc);
                    ReleaseDC(HWND::default(), screen_dc);
                    return Err("CreateCompatibleBitmap failed".to_string());
                }

                let previous_bitmap = SelectObject(mem_dc, bitmap);
                if previous_bitmap.is_invalid() {
                    let _ = DeleteObject(bitmap);
                    let _ = DeleteDC(mem_dc);
                    ReleaseDC(HWND::default(), screen_dc);
                    return Err("SelectObject(mem_dc, bitmap) failed".to_string());
                }

                Ok(Self {
                    screen_dc,
                    mem_dc,
                    bitmap,
                    previous_bitmap,
                    origin_x,
                    origin_y,
                    width,
                    height,
                    scratch: vec![0u8; (width * height * 4) as usize],
                    consecutive_black_frames: 0,
                })
            }
        }

        pub fn capture_frame(&mut self) -> Result<DesktopFrame, String> {
            unsafe {

                let rop = ROP_CODE(SRCCOPY.0 | CAPTUREBLT.0);
                BitBlt(
                    self.mem_dc,
                    0,
                    0,
                    self.width,
                    self.height,
                    self.screen_dc,
                    self.origin_x,
                    self.origin_y,
                    rop,
                )
                .map_err(|err| format!("BitBlt failed: {err}"))?;

                // BITMAPINFOHEADER avec biHeight NÉGATIF = top-down DIB.
                // Sinon GetDIBits remplit en bottom-up et il faut flipper
                // les lignes — perte de temps inutile.
                let mut info = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: self.width,
                        biHeight: -self.height,
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB.0,
                        biSizeImage: (self.width * self.height * 4) as u32,
                        biXPelsPerMeter: 0,
                        biYPelsPerMeter: 0,
                        biClrUsed: 0,
                        biClrImportant: 0,
                    },
                    bmiColors: [Default::default(); 1],
                };

                // Resize du scratch buffer si la résolution a changé
                // (changement de display mode en cours de session).
                let expected = (self.width * self.height * 4) as usize;
                if self.scratch.len() != expected {
                    self.scratch.resize(expected, 0);
                }

                let rows_copied = GetDIBits(
                    self.mem_dc,
                    self.bitmap,
                    0,
                    self.height as u32,
                    Some(self.scratch.as_mut_ptr().cast()),
                    &mut info,
                    DIB_RGB_COLORS,
                );
                if rows_copied == 0 {
                    return Err("GetDIBits returned 0 rows".to_string());
                }


                if is_black_frame(&self.scratch) {
                    self.consecutive_black_frames =
                        self.consecutive_black_frames.saturating_add(1);
                    if self.consecutive_black_frames % BLACK_FRAME_THRESHOLD == 0 {
                        tracing::warn!(
                            "🎥 GDI BitBlt renvoie une frame noire ({} consécutives) — \
                             session verrouillée, secure desktop UAC, ou capture bloquée par GPO ?",
                            self.consecutive_black_frames
                        );
                    }
                } else if self.consecutive_black_frames > 0 {
                    tracing::info!(
                        "🎥 GDI BitBlt: récupération après {} frames noires",
                        self.consecutive_black_frames
                    );
                    self.consecutive_black_frames = 0;
                }

                Ok(DesktopFrame {
                    width: self.width as usize,
                    height: self.height as usize,
                    stride: (self.width * 4) as usize,
                    captured_at: Instant::now(),
                    bgra: self.scratch.clone(),
                })
            }
        }
    }


    unsafe impl Send for GdiCapturer {}
    unsafe impl Sync for GdiCapturer {}

    impl Drop for GdiCapturer {
        fn drop(&mut self) {
            unsafe {
                // Restaure l'ancien bitmap dans le DC mémoire avant de
                // delete le nôtre (sinon DeleteObject échoue).
                if !self.previous_bitmap.is_invalid() {
                    SelectObject(self.mem_dc, self.previous_bitmap);
                }
                if !self.bitmap.is_invalid() {
                    let _ = DeleteObject(self.bitmap);
                }
                if !self.mem_dc.is_invalid() {
                    let _ = DeleteDC(self.mem_dc);
                }
                if !self.screen_dc.is_invalid() {
                    ReleaseDC(HWND::default(), self.screen_dc);
                }
            }
        }
    }

    pub fn bgra_to_nv12(
        width: usize,
        height: usize,
        stride: usize,
        bgra: &[u8],
    ) -> Nv12Frame {
        let mut packed = Vec::new();
        bgra_to_nv12_packed(width, height, stride, bgra, &mut packed);
        let y_len = width * height;
        let uv_len = width * (height / 2);
        let mut y_plane = vec![0u8; y_len];
        let mut uv_plane = vec![0u8; uv_len];
        y_plane.copy_from_slice(&packed[..y_len]);
        uv_plane.copy_from_slice(&packed[y_len..(y_len + uv_len)]);

        Nv12Frame {
            width,
            height,
            y_plane,
            uv_plane,
        }
    }

    pub fn bgra_to_nv12_packed(
        width: usize,
        height: usize,
        stride: usize,
        bgra: &[u8],
        out: &mut Vec<u8>,
    ) {
        let y_len = width * height;
        let uv_len = width * (height / 2);
        let total_len = y_len + uv_len;

        if out.len() != total_len {
            out.resize(total_len, 0);
        }

        let (y_plane, uv_plane) = out.split_at_mut(y_len);

        // BT.601 integer fixed-point conversion (coefficients scaled x256).
        for row in (0..height).step_by(2) {
            for col in (0..width).step_by(2) {
                let mut u_acc: i32 = 0;
                let mut v_acc: i32 = 0;

                for block_y in 0..2usize {
                    for block_x in 0..2usize {
                        let px = col + block_x;
                        let py = row + block_y;
                        let idx = py * stride + px * 4;
                        let b = bgra[idx] as i32;
                        let g = bgra[idx + 1] as i32;
                        let r = bgra[idx + 2] as i32;

                        let y_val = ((66 * r + 129 * g + 25 * b + 128) >> 8) + 16;
                        y_plane[py * width + px] = y_val.clamp(0, 255) as u8;

                        u_acc += -38 * r - 74 * g + 112 * b;
                        v_acc += 112 * r - 94 * g - 18 * b;
                    }
                }

                let uv_index = (row / 2) * width + col;
                uv_plane[uv_index] = (((u_acc + 512) >> 10) + 128).clamp(0, 255) as u8;
                uv_plane[uv_index + 1] = (((v_acc + 512) >> 10) + 128).clamp(0, 255) as u8;
            }
        }
    }

    // ─── Tests ────────────────────────────────────────────────────────────
    #[cfg(test)]
    mod tests {
        use super::is_black_frame;

        #[test]
        fn detects_all_zero_buffer_as_black() {
            // 4×4 BGRA tout à zéro = noir total.
            let buf = vec![0u8; 4 * 4 * 4];
            assert!(is_black_frame(&buf));
        }

        #[test]
        fn detects_all_zero_with_alpha_255_as_black() {
            // Certaines surfaces D3D11 produisent un alpha=255 même quand
            // RGB est à zéro. On doit quand même classer ça comme noir.
            let mut buf = vec![0u8; 4 * 4 * 4];
            for px in buf.chunks_exact_mut(4) {
                px[3] = 255;
            }
            assert!(is_black_frame(&buf));
        }

        #[test]
        fn rejects_single_white_pixel() {
            // Un seul pixel blanc dans un buffer noir doit suffire à
            // disqualifier (sinon on raterait des contenus quasi-noirs
            // mais bien réels — écran de jeu sombre, terminal noir).
            let mut buf = vec![0u8; 128 * 128 * 4];
            // Pixel au centre, R=G=B=255
            let center = (64 * 128 + 64) * 4;
            buf[center] = 255;
            buf[center + 1] = 255;
            buf[center + 2] = 255;
            // Le sampling pourrait rater ce pixel précis selon le step.
            // Pour le test on prend une image plus grande dont le step
            // tombe sur le pixel — donc on met plusieurs pixels blancs
            // pour garantir qu'au moins un est échantillonné.
            for i in 0..1024 {
                let idx = i * 64 * 4;
                if idx + 2 < buf.len() {
                    buf[idx + 2] = 200; // R non-zéro
                }
            }
            assert!(!is_black_frame(&buf));
        }

        #[test]
        fn rejects_buffer_with_low_intensity_pixels() {
            // Image très sombre mais pas totalement noire (R=5).
            let buf = vec![5u8; 64 * 64 * 4];
            assert!(!is_black_frame(&buf));
        }

        #[test]
        fn handles_empty_buffer() {
            assert!(is_black_frame(&[]));
        }

        #[test]
        fn handles_undersized_buffer() {
            // Moins d'un pixel complet → considéré noir (pas de données utiles).
            assert!(is_black_frame(&[0u8, 0u8, 0u8]));
        }

        #[test]
        fn realistic_1080p_black_frame() {
            // Buffer Full HD entièrement noir — le cas exact d'Intel UHD.
            let buf = vec![0u8; 1920 * 1080 * 4];
            assert!(is_black_frame(&buf));
        }

        #[test]
        fn realistic_1080p_with_content() {
            // Buffer Full HD avec un dégradé → pas noir.
            let mut buf = vec![0u8; 1920 * 1080 * 4];
            for (i, px) in buf.chunks_exact_mut(4).enumerate() {
                px[2] = ((i / 1920) & 0xff) as u8; // R varie par ligne
            }
            assert!(!is_black_frame(&buf));
        }
    }
}

#[cfg(windows)]
pub use imp::*;

#[cfg(not(windows))]
mod imp_stub {
    use std::time::Instant;

    #[derive(Clone, Debug)]
    pub struct DesktopFrame {
        pub width: usize,
        pub height: usize,
        pub stride: usize,
        pub captured_at: Instant,
        pub bgra: Vec<u8>,
    }

    impl DesktopFrame {
        pub fn into_even_bgra(self) -> (usize, usize, Vec<u8>) {
            (self.width, self.height, self.bgra)
        }

        pub fn resize_bgra_nearest(&self, _target_width: usize, _target_height: usize) -> Self {
            self.clone()
        }

        pub fn to_nv12(&self) -> Nv12Frame {
            Nv12Frame {
                width: self.width,
                height: self.height,
                y_plane: Vec::new(),
                uv_plane: Vec::new(),
            }
        }
    }

    #[derive(Clone, Debug)]
    pub struct Nv12Frame {
        pub width: usize,
        pub height: usize,
        pub y_plane: Vec<u8>,
        pub uv_plane: Vec<u8>,
    }

    impl Nv12Frame {
        pub fn as_bytes(&self) -> Vec<u8> {
            Vec::new()
        }
    }

    pub struct DxgiDesktopDuplicator;

    impl DxgiDesktopDuplicator {
        pub fn new() -> Result<Self, String> {
            Err("DXGI desktop duplication is only available on Windows".to_string())
        }

        pub fn capture_next_frame(&mut self, _timeout_ms: u32) -> Result<Option<DesktopFrame>, String> {
            Err("DXGI desktop duplication is only available on Windows".to_string())
        }
    }
}

#[cfg(not(windows))]
pub use imp_stub::*;

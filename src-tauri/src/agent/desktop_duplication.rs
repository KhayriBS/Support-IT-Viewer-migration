#[cfg(windows)]
mod imp {
    use std::env;
    use std::time::Instant;

    use windows::core::Interface;
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
    use screenshots::Screen;

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

    pub struct DxgiDesktopDuplicator {
        device: Option<ID3D11Device>,
        context: Option<ID3D11DeviceContext>,
        duplication: Option<IDXGIOutputDuplication>,
        fallback_screen: Option<Screen>,
        staging_texture: Option<ID3D11Texture2D>,
        cached_width: u32,
        cached_height: u32,
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
                                    "DXGI selected adapter #{adapter_index} ({adapter_name}) output #{output_index} (monitor ordinal {}): {}x{}",
                                    attached_output_ordinal,
                                    width,
                                    height
                                );

                                return Ok(Self {
                                    device: Some(device),
                                    context: Some(context),
                                    duplication: Some(duplication),
                                    fallback_screen: None,
                                    staging_texture: None,
                                    cached_width: 0,
                                    cached_height: 0,
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
                    "DXGI indisponible, fallback screenshots activé sur display index {} id {} ({}x{})",
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
                })
            }
        }

        pub fn capture_next_frame(
            &mut self,
            timeout_ms: u32,
        ) -> Result<Option<DesktopFrame>, String> {
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

            unsafe {
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

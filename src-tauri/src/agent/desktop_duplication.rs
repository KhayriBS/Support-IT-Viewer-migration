#[cfg(windows)]
mod imp {
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
    };

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
        device: ID3D11Device,
        context: ID3D11DeviceContext,
        duplication: IDXGIOutputDuplication,
        staging_texture: Option<ID3D11Texture2D>,
        cached_width: u32,
        cached_height: u32,
    }

    impl DxgiDesktopDuplicator {
        pub fn new() -> Result<Self, String> {
            unsafe {
                let factory: IDXGIFactory1 =
                    CreateDXGIFactory1().map_err(|err| format!("CreateDXGIFactory1 failed: {err}"))?;
                let adapter: IDXGIAdapter1 = factory
                    .EnumAdapters1(0)
                    .map_err(|err| format!("EnumAdapters1 failed: {err}"))?;
                let base_adapter: IDXGIAdapter = adapter
                    .cast()
                    .map_err(|err| format!("IDXGIAdapter1 cast failed: {err}"))?;
                let output = adapter
                    .EnumOutputs(0)
                    .map_err(|err| format!("EnumOutputs failed: {err}"))?;
                let output1: IDXGIOutput1 = output
                    .cast()
                    .map_err(|err| format!("IDXGIOutput1 cast failed: {err}"))?;

                let feature_levels: [D3D_FEATURE_LEVEL; 2] =
                    [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
                let mut device = None;
                let mut context = None;

                D3D11CreateDevice(
                    Some(&base_adapter),
                    D3D_DRIVER_TYPE_UNKNOWN,
                    None,
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    Some(&feature_levels),
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut context),
                )
                .map_err(|err| format!("D3D11CreateDevice failed: {err}"))?;

                let device = device.ok_or_else(|| "D3D11 device unavailable".to_string())?;
                let context = context.ok_or_else(|| "D3D11 context unavailable".to_string())?;
                let duplication = output1
                    .DuplicateOutput(&device)
                    .map_err(|err| format!("DuplicateOutput failed: {err}"))?;

                Ok(Self {
                    device,
                    context,
                    duplication,
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
            unsafe {
                let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
                let mut desktop_resource: Option<IDXGIResource> = None;
                match self
                    .duplication
                    .AcquireNextFrame(timeout_ms, &mut frame_info, &mut desktop_resource)
                {
                    Ok(()) => {}
                    Err(err) if err.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                        return Ok(None);
                    }
                    Err(err) => {
                        return Err(format!("AcquireNextFrame failed: {err}"));
                    }
                }

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

                self.context.CopyResource(&staging_resource, &source_resource);

                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                self.context
                    .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                    .map_err(|err| format!("Map staging texture failed: {err}"))?;

                let width = desc.Width as usize;
                let height = desc.Height as usize;
                let row_pitch = mapped.RowPitch as usize;
                let byte_len = row_pitch.saturating_mul(height);
                let src = std::slice::from_raw_parts(mapped.pData.cast::<u8>(), byte_len);
                let mut bgra = vec![0u8; width * height * 4];
                for y in 0..height {
                    let src_row = &src[y * row_pitch..y * row_pitch + width * 4];
                    let dst_row = &mut bgra[y * width * 4..y * width * 4 + width * 4];
                    dst_row.copy_from_slice(src_row);
                }

                self.context.Unmap(staging, 0);
                let _ = self.duplication.ReleaseFrame();

                Ok(Some(DesktopFrame {
                    width,
                    height,
                    stride: width * 4,
                    captured_at: Instant::now(),
                    bgra,
                }))
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
            self.device
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
        let mut y_plane = vec![0u8; width * height];
        let mut uv_plane = vec![0u8; width * (height / 2)];

        for y in (0..height).step_by(2) {
            for x in (0..width).step_by(2) {
                let mut u_acc = 0.0f32;
                let mut v_acc = 0.0f32;

                for block_y in 0..2 {
                    for block_x in 0..2 {
                        let px = x + block_x;
                        let py = y + block_y;
                        let idx = py * stride + px * 4;
                        let b = bgra[idx] as f32;
                        let g = bgra[idx + 1] as f32;
                        let r = bgra[idx + 2] as f32;

                        let y_value = (0.257 * r + 0.504 * g + 0.098 * b + 16.0)
                            .round()
                            .clamp(0.0, 255.0) as u8;
                        y_plane[py * width + px] = y_value;

                        u_acc += (-0.148 * r - 0.291 * g + 0.439 * b + 128.0).clamp(0.0, 255.0);
                        v_acc += (0.439 * r - 0.368 * g - 0.071 * b + 128.0).clamp(0.0, 255.0);
                    }
                }

                let uv_index = (y / 2) * width + x;
                uv_plane[uv_index] = (u_acc / 4.0).round().clamp(0.0, 255.0) as u8;
                uv_plane[uv_index + 1] = (v_acc / 4.0).round().clamp(0.0, 255.0) as u8;
            }
        }

        Nv12Frame {
            width,
            height,
            y_plane,
            uv_plane,
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

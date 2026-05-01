use std::time::{Duration, Instant};
use std::env;

use screenshots::Screen;

use super::ScreenCapturer;
use crate::agent::desktop_duplication::DesktopFrame;

#[cfg(windows)]
mod winrt {
    use std::time::Instant;

    use windows::core::{Interface, Result as WinResult};
    use windows::Foundation::TypedEventHandler;
    use windows::Graphics::Capture::{
        Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
    };
    use windows::Graphics::DirectX::Direct3D11::{IDirect3DDevice, IDirect3DSurface};
    use windows::Graphics::DirectX::DirectXPixelFormat;
    use windows::Win32::Foundation::{E_BOUNDS, E_FAIL, HWND};
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
        D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
        D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Resource, ID3D11Texture2D,
    };
    use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
    use windows::Win32::Graphics::Dxgi::IDXGIDevice;
    use windows::Win32::Graphics::Gdi::{MonitorFromWindow, MONITOR_DEFAULTTOPRIMARY};
    use windows::Win32::System::WinRT::Direct3D11::{
        CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
    };
    use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
    use windows::Win32::UI::WindowsAndMessaging::GetDesktopWindow;

    use crate::agent::desktop_duplication::DesktopFrame;

    pub struct NativeWgcCapturer {
        d3d_device: ID3D11Device,
        d3d_context: ID3D11DeviceContext,
        frame_pool: Direct3D11CaptureFramePool,
        _session: GraphicsCaptureSession,
        staging_texture: Option<ID3D11Texture2D>,
        cached_width: u32,
        cached_height: u32,
    }

    impl NativeWgcCapturer {
        pub fn new() -> WinResult<Self> {
            unsafe {
                let mut device = None;
                let mut context = None;
                D3D11CreateDevice(
                    None,
                    windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE,
                    None,
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut context),
                )?;

                let d3d_device = device.ok_or_else(|| windows::core::Error::new(E_FAIL, "D3D11 device unavailable"))?;
                let d3d_context = context.ok_or_else(|| windows::core::Error::new(E_FAIL, "D3D11 context unavailable"))?;

                let dxgi_device: IDXGIDevice = d3d_device.cast()?;
                let inspectable = CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device)?;
                let winrt_d3d: IDirect3DDevice = inspectable.cast()?;

                let desktop_hwnd: HWND = GetDesktopWindow();
                let monitor = MonitorFromWindow(desktop_hwnd, MONITOR_DEFAULTTOPRIMARY);
                if monitor.is_invalid() {
                    return Err(windows::core::Error::new(E_FAIL, "Primary monitor unavailable"));
                }

                let item_interop: IGraphicsCaptureItemInterop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
                let item: GraphicsCaptureItem = item_interop.CreateForMonitor(monitor)?;
                let size = item.Size()?;
                if size.Width <= 1 || size.Height <= 1 {
                    return Err(windows::core::Error::new(E_FAIL, "Invalid WGC capture item size"));
                }

                let frame_pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
                    &winrt_d3d,
                    DirectXPixelFormat::B8G8R8A8UIntNormalized,
                    2,
                    size,
                )?;

                let session = frame_pool.CreateCaptureSession(&item)?;
                session.SetIsCursorCaptureEnabled(true)?;

                // We use polling (`TryGetNextFrame`) in the capture loop. Attach a no-op callback
                // so the runtime keeps producing frames for this pool.
                let handler = TypedEventHandler::<Direct3D11CaptureFramePool, windows::core::IInspectable>::new(
                    move |_, _| Ok(())
                );
                let _token = frame_pool.FrameArrived(&handler)?;

                session.StartCapture()?;

                Ok(Self {
                    d3d_device,
                    d3d_context,
                    frame_pool,
                    _session: session,
                    staging_texture: None,
                    cached_width: 0,
                    cached_height: 0,
                })
            }
        }

        pub fn capture_next_frame(&mut self) -> Result<Option<DesktopFrame>, String> {
            let frame = match self.frame_pool.TryGetNextFrame() {
                Ok(frame) => frame,
                Err(err) if err.code() == E_BOUNDS => return Ok(None),
                Err(err) => return Err(format!("WGC TryGetNextFrame failed: {err}")),
            };

            let surface: IDirect3DSurface = frame.Surface().map_err(|err| format!("WGC frame surface unavailable: {err}"))?;
            let access: IDirect3DDxgiInterfaceAccess = surface
                .cast()
                .map_err(|err| format!("WGC surface interop cast failed: {err}"))?;

            let texture: ID3D11Texture2D = unsafe {
                access
                    .GetInterface()
                    .map_err(|err| format!("WGC GetInterface<ID3D11Texture2D> failed: {err}"))?
            };

            let mut desc = D3D11_TEXTURE2D_DESC::default();
            unsafe {
                texture.GetDesc(&mut desc);
            }

            self.ensure_staging_texture(&desc)?;

            let staging = self
                .staging_texture
                .as_ref()
                .ok_or_else(|| "WGC staging texture unavailable".to_string())?;

            let source_resource: ID3D11Resource = texture
                .cast()
                .map_err(|err| format!("WGC source cast failed: {err}"))?;
            let staging_resource: ID3D11Resource = staging
                .cast()
                .map_err(|err| format!("WGC staging cast failed: {err}"))?;

            unsafe {
                self.d3d_context.CopyResource(&staging_resource, &source_resource);
            }

            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            unsafe {
                self.d3d_context
                    .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                    .map_err(|err| format!("WGC map staging failed: {err}"))?;
            }

            let width = desc.Width as usize;
            let height = desc.Height as usize;
            let row_pitch = mapped.RowPitch as usize;
            let row_bytes = width.saturating_mul(4);
            let total_bytes = row_pitch.saturating_mul(height);
            let src = unsafe { std::slice::from_raw_parts(mapped.pData.cast::<u8>(), total_bytes) };

            let bgra = if row_pitch == row_bytes {
                src[..row_bytes * height].to_vec()
            } else {
                let mut out = vec![0u8; row_bytes * height];
                for y in 0..height {
                    let src_start = y * row_pitch;
                    let dst_start = y * row_bytes;
                    out[dst_start..dst_start + row_bytes]
                        .copy_from_slice(&src[src_start..src_start + row_bytes]);
                }
                out
            };

            unsafe {
                self.d3d_context.Unmap(staging, 0);
            }

            Ok(Some(DesktopFrame {
                width,
                height,
                stride: width * 4,
                captured_at: Instant::now(),
                bgra,
            }))
        }

        fn ensure_staging_texture(&mut self, source_desc: &D3D11_TEXTURE2D_DESC) -> Result<(), String> {
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
                Format: source_desc.Format,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };

            let mut staging = None;
            unsafe {
                self.d3d_device
                    .CreateTexture2D(&staging_desc, None, Some(&mut staging))
                    .map_err(|err| format!("WGC CreateTexture2D staging failed: {err}"))?;
            }

            self.staging_texture = staging;
            self.cached_width = source_desc.Width;
            self.cached_height = source_desc.Height;
            Ok(())
        }
    }
}

pub struct WgcCapturer {
    #[cfg(windows)]
    native: Option<winrt::NativeWgcCapturer>,
    fallback: Option<Screen>,
}

fn read_monitor_index() -> Option<usize> {
    env::var("LUMIERE_CAPTURE_MONITOR_INDEX")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
}

fn select_screen_for_index(screens: &[Screen], monitor_index: Option<usize>) -> Result<Screen, String> {
    if screens.is_empty() {
        return Err("No display available for WGC fallback capture".to_string());
    }

    if let Some(index) = monitor_index {
        return screens
            .get(index)
            .copied()
            .ok_or_else(|| format!("Requested monitor index {index} is out of range ({} displays)", screens.len()));
    }

    screens
        .iter()
        .copied()
        .find(|s| s.display_info.is_primary)
        .or_else(|| screens.first().copied())
        .ok_or_else(|| "No display available for WGC fallback capture".to_string())
}

impl WgcCapturer {
    pub fn new() -> Result<Self, String> {
        let monitor_index = read_monitor_index();

        #[cfg(windows)]
        {
            // Current native WGC path captures the primary monitor; if a non-primary index is requested,
            // route directly to screenshots fallback so monitor selection is honored deterministically.
            if monitor_index.unwrap_or(0) == 0 {
                match winrt::NativeWgcCapturer::new() {
                    Ok(native) => {
                        println!("WGC native WinRT capture initialized (monitor index: 0)");
                        return Ok(Self {
                            native: Some(native),
                            fallback: None,
                        });
                    }
                    Err(err) => {
                        eprintln!("WGC native initialization failed: {err}; falling back to screenshots path");
                    }
                }
            } else {
                println!(
                    "WGC monitor index {} requested; using screenshots fallback for explicit monitor targeting",
                    monitor_index.unwrap_or(0)
                );
            }
        }

        let screens = Screen::all().map_err(|err| format!("WGC fallback screen discovery failed: {err}"))?;
        let screen = select_screen_for_index(&screens, monitor_index)?;

        let selected_index = screens
            .iter()
            .position(|candidate| candidate.display_info.id == screen.display_info.id)
            .unwrap_or(0);

        println!(
            "WGC fallback using display index {} id {} ({}x{})",
            selected_index,
            screen.display_info.id,
            screen.display_info.width,
            screen.display_info.height
        );

        Ok(Self {
            #[cfg(windows)]
            native: None,
            fallback: Some(screen),
        })
    }
}

impl ScreenCapturer for WgcCapturer {
    fn capture_next_frame(&mut self, timeout_ms: u32) -> Result<Option<DesktopFrame>, String> {
        #[cfg(windows)]
        if let Some(native) = self.native.as_mut() {
            let start = Instant::now();
            loop {
                if let Some(frame) = native.capture_next_frame()? {
                    return Ok(Some(frame));
                }
                if timeout_ms == 0 || start.elapsed() >= Duration::from_millis(timeout_ms as u64) {
                    return Ok(None);
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }

        let Some(screen) = self.fallback else {
            return Err("WGC capturer is not initialized".to_string());
        };

        if timeout_ms > 0 {
            std::thread::sleep(Duration::from_millis(timeout_ms.min(33) as u64));
        }

        let rgba = screen
            .capture()
            .map_err(|err| format!("WGC fallback capture failed: {err}"))?;

        let width = rgba.width() as usize;
        let height = rgba.height() as usize;
        let mut bgra = rgba.into_raw();
        for px in bgra.chunks_exact_mut(4) {
            px.swap(0, 2);
        }

        Ok(Some(DesktopFrame {
            width,
            height,
            stride: width * 4,
            captured_at: Instant::now(),
            bgra,
        }))
    }
}

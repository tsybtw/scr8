//! Fast capture with DXGI Desktop Duplication.
//!
//! Each monitor's duplication stays open, and every capture takes the latest
//! desktop image straight from the GPU and copies back only the requested
//! area. When it isn't available (Remote Desktop, no GPU, the secure desktop,
//! rotated screens) the caller falls back to GDI; a failed setup is retried
//! after a pause.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_MODE_ROTATION_IDENTITY, DXGI_MODE_ROTATION_UNSPECIFIED,
    DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_NOT_FOUND, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
    IDXGIAdapter1, IDXGIFactory1, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource,
};
use windows::core::Interface;

use super::Frame;
use crate::config::Region;

/// How long to wait before trying Desktop Duplication again after it failed.
const RETRY_AFTER: Duration = Duration::from_secs(5);

struct Output {
    /// Virtual-desktop pixels.
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    dupl: IDXGIOutputDuplication,
    /// Copy of the latest desktop image; kept because the duplication only
    /// hands out new frames when the screen changes.
    latest: Option<ID3D11Texture2D>,
}

struct Adapter {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    outputs: Vec<Output>,
}

struct Dda {
    adapters: Vec<Adapter>,
}

enum State {
    Ready(Dda),
    Failed(Instant),
}

// The D3D objects are used under the mutex only.
unsafe impl Send for State {}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// Opens the duplications ahead of the first capture (it takes a while).
pub fn warm_up() {
    let mut state = STATE.lock().unwrap();
    if state.is_none() {
        *state = Some(match Dda::new() {
            Ok(mut d) => {
                // Take the first frame now; it can take a while to arrive.
                for a in &mut d.adapters {
                    for out in &mut a.outputs {
                        let _ = refresh(&a.device, &a.ctx, out, None);
                    }
                }
                State::Ready(d)
            }
            Err(_) => State::Failed(Instant::now()),
        });
    }
}

/// Captures `r`, or `None` when Desktop Duplication can't be used right now.
pub fn capture(r: Region) -> Option<Frame> {
    let mut state = STATE.lock().unwrap();
    let retry = match &*state {
        None => true,
        Some(State::Failed(at)) => at.elapsed() >= RETRY_AFTER,
        Some(State::Ready(_)) => false,
    };
    if retry {
        *state = Some(match Dda::new() {
            Ok(d) => State::Ready(d),
            Err(_) => State::Failed(Instant::now()),
        });
    }
    let Some(State::Ready(dda)) = state.as_mut() else {
        return None;
    };
    match dda.capture(r) {
        Ok(frame) => Some(frame),
        Err(_) => {
            // Lost (mode change, secure desktop, …): rebuild on a later try.
            *state = Some(State::Failed(Instant::now()));
            None
        }
    }
}

impl Dda {
    fn new() -> windows::core::Result<Self> {
        unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
            let mut adapters = Vec::new();
            for a in 0.. {
                let adapter: IDXGIAdapter1 = match factory.EnumAdapters1(a) {
                    Ok(x) => x,
                    Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                    Err(e) => return Err(e),
                };
                let mut outs = Vec::new();
                for o in 0.. {
                    match adapter.EnumOutputs(o) {
                        Ok(out) => outs.push(out),
                        Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                        Err(e) => return Err(e),
                    }
                }
                if outs.is_empty() {
                    continue;
                }
                let mut device = None;
                let mut ctx = None;
                D3D11CreateDevice(
                    &adapter,
                    D3D_DRIVER_TYPE_UNKNOWN,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut ctx),
                )?;
                let (Some(device), Some(ctx)) = (device, ctx) else {
                    continue;
                };
                let mut outputs = Vec::new();
                for out in outs {
                    let desc = out.GetDesc()?;
                    if !desc.AttachedToDesktop.as_bool() {
                        continue;
                    }
                    let out1: IDXGIOutput1 = out.cast()?;
                    let dupl = out1.DuplicateOutput(&device)?;
                    let d = dupl.GetDesc();
                    // Rotated screens would need their pixels turned; GDI does it.
                    if d.Rotation != DXGI_MODE_ROTATION_IDENTITY
                        && d.Rotation != DXGI_MODE_ROTATION_UNSPECIFIED
                    {
                        return Err(windows::core::Error::from_hresult(
                            windows::Win32::Foundation::E_NOTIMPL,
                        ));
                    }
                    let c = desc.DesktopCoordinates;
                    outputs.push(Output {
                        left: c.left,
                        top: c.top,
                        right: c.right,
                        bottom: c.bottom,
                        dupl,
                        latest: None,
                    });
                }
                if !outputs.is_empty() {
                    adapters.push(Adapter {
                        device,
                        ctx,
                        outputs,
                    });
                }
            }
            if adapters.is_empty() {
                return Err(windows::core::Error::from_hresult(
                    windows::Win32::Foundation::E_FAIL,
                ));
            }
            Ok(Self { adapters })
        }
    }

    fn capture(&mut self, r: Region) -> windows::core::Result<Frame> {
        let (rl, rt) = (r.x, r.y);
        let (rr, rb) = (r.x + r.w as i32, r.y + r.h as i32);
        let stride = r.w as usize * 4;
        let mut buf = vec![0u8; stride * r.h as usize];
        for adapter in &mut self.adapters {
            for out in &mut adapter.outputs {
                let (l, t) = (rl.max(out.left), rt.max(out.top));
                let (rgt, btm) = (rr.min(out.right), rb.min(out.bottom));
                if rgt <= l || btm <= t {
                    continue;
                }
                let (w, h) = ((rgt - l) as u32, (btm - t) as u32);
                let staging = staging_texture(&adapter.device, w, h)?;
                let src_box = D3D11_BOX {
                    left: (l - out.left) as u32,
                    top: (t - out.top) as u32,
                    front: 0,
                    right: (rgt - out.left) as u32,
                    bottom: (btm - out.top) as u32,
                    back: 1,
                };
                refresh(
                    &adapter.device,
                    &adapter.ctx,
                    out,
                    Some((&staging, &src_box)),
                )?;
                unsafe {
                    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                    adapter
                        .ctx
                        .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
                    let row = w as usize * 4;
                    let src = mapped.pData as *const u8;
                    for y in 0..h as usize {
                        let from =
                            std::slice::from_raw_parts(src.add(y * mapped.RowPitch as usize), row);
                        let at = (t - rt) as usize + y;
                        let off = at * stride + (l - rl) as usize * 4;
                        buf[off..off + row].copy_from_slice(from);
                    }
                    adapter.ctx.Unmap(&staging, 0);
                }
            }
        }
        Ok(Frame {
            width: r.w,
            height: r.h,
            bgrx: buf,
        })
    }
}

/// Brings `out.latest` up to date and, if given, copies `area` of the
/// current desktop image into `staging`.
///
/// With a new frame the area is copied from it first and only then the whole
/// frame into `latest`, so reading `staging` back doesn't wait for the big
/// copy.
fn refresh(
    device: &ID3D11Device,
    ctx: &ID3D11DeviceContext,
    out: &mut Output,
    area: Option<(&ID3D11Texture2D, &D3D11_BOX)>,
) -> windows::core::Result<()> {
    // Right after opening, the first frame can take a moment to arrive.
    let timeout = if out.latest.is_some() { 0 } else { 200 };
    let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
    let mut resource: Option<IDXGIResource> = None;
    match unsafe { out.dupl.AcquireNextFrame(timeout, &mut info, &mut resource) } {
        Ok(()) => {
            let result = (|| {
                let frame: ID3D11Texture2D = resource
                    .ok_or_else(|| {
                        windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL)
                    })?
                    .cast()?;
                let mut desc = D3D11_TEXTURE2D_DESC::default();
                unsafe { frame.GetDesc(&mut desc) };
                // Frames are always 8-bit BGRA with this API; anything else
                // goes to GDI rather than being misread.
                if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
                    return Err(windows::core::Error::from_hresult(
                        windows::Win32::Foundation::E_NOTIMPL,
                    ));
                }
                if let Some((staging, b)) = area {
                    unsafe { ctx.CopySubresourceRegion(staging, 0, 0, 0, 0, &frame, 0, Some(b)) };
                }
                if out.latest.is_none() {
                    desc.Usage = D3D11_USAGE_DEFAULT;
                    desc.BindFlags = 0;
                    desc.CPUAccessFlags = 0;
                    desc.MiscFlags = 0;
                    let mut tex = None;
                    unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex))? };
                    out.latest = tex;
                }
                if let Some(latest) = &out.latest {
                    unsafe { ctx.CopyResource(latest, &frame) };
                }
                Ok(())
            })();
            unsafe { out.dupl.ReleaseFrame()? };
            result?;
        }
        // Nothing changed since the last frame: `latest` is current.
        Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT && out.latest.is_some() => {
            if let (Some((staging, b)), Some(latest)) = (area, &out.latest) {
                unsafe { ctx.CopySubresourceRegion(staging, 0, 0, 0, 0, latest, 0, Some(b)) };
            }
        }
        Err(e) => return Err(e),
    }
    Ok(())
}

fn staging_texture(
    device: &ID3D11Device,
    w: u32,
    h: u32,
) -> windows::core::Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };
    let mut tex = None;
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex))? };
    tex.ok_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL))
}

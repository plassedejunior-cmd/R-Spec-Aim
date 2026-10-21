//! Captura de mouse bruto. O app só conhece o trait `MouseSource`; a implementação
//! Windows (Raw Input) fica isolada para permitir portar depois (seção 2 da spec).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub use rspec_core::RawSample;

/// Contadores que a thread de captura atualiza e a UI lê.
#[derive(Default, Debug)]
pub struct CaptureStats {
    pub events: AtomicU64,
    /// Eventos com MOUSE_MOVE_ABSOLUTE (tablet, RDP, VM): descartados.
    pub absolute: AtomicU64,
}

impl CaptureStats {
    pub fn absolute_seen(&self) -> bool {
        self.absolute.load(Ordering::Relaxed) > 0
    }
}

/// Configuração do ponteiro do sistema, só para informar o usuário.
/// O Raw Input ignora as duas coisas.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemMouseInfo {
    /// "Aumentar precisão do ponteiro" (aceleração do Windows).
    pub enhance_precision: bool,
    /// Velocidade do ponteiro, 1 a 20 (10 é o neutro).
    pub speed: u32,
}

pub trait MouseSource {
    /// Move para `out` todas as amostras capturadas desde a última chamada.
    fn drain(&mut self, out: &mut Vec<RawSample>);
    fn stats(&self) -> Arc<CaptureStats>;
    fn system_info(&self) -> Option<SystemMouseInfo>;
    /// Nome do backend, para o arquivo de sessão.
    fn backend(&self) -> &'static str;
}

/// Abre a fonte de input da plataforma.
pub fn open() -> Result<Box<dyn MouseSource>, String> {
    #[cfg(windows)]
    {
        windows::RawInputSource::start().map(|s| Box::new(s) as Box<dyn MouseSource>)
    }
    #[cfg(not(windows))]
    {
        Err("captura bruta só existe no Windows 11 por enquanto".into())
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use rspec_core::buttons;
    use std::sync::mpsc::{channel, Receiver, Sender};
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL};
    use windows_sys::Win32::UI::Input::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    pub struct RawInputSource {
        rx: Receiver<RawSample>,
        stats: Arc<CaptureStats>,
    }

    impl RawInputSource {
        pub fn start() -> Result<Self, String> {
            let (tx, rx) = channel();
            let (ready_tx, ready_rx) = channel();
            let stats = Arc::new(CaptureStats::default());
            let st = stats.clone();
            std::thread::Builder::new()
                .name("raw-input".into())
                .spawn(move || unsafe { run(tx, st, ready_tx) })
                .map_err(|e| e.to_string())?;
            ready_rx.recv().map_err(|_| "thread de captura morreu".to_string())??;
            Ok(Self { rx, stats })
        }
    }

    impl MouseSource for RawInputSource {
        fn drain(&mut self, out: &mut Vec<RawSample>) {
            out.extend(self.rx.try_iter());
        }
        fn stats(&self) -> Arc<CaptureStats> {
            self.stats.clone()
        }
        fn system_info(&self) -> Option<SystemMouseInfo> {
            unsafe {
                let mut acc = [0i32; 3];
                let mut speed = 0u32;
                let ok1 = SystemParametersInfoW(SPI_GETMOUSE, 0, acc.as_mut_ptr().cast(), 0) != 0;
                let ok2 = SystemParametersInfoW(SPI_GETMOUSESPEED, 0, (&mut speed as *mut u32).cast(), 0) != 0;
                // acc[2] != 0 => "aumentar precisão do ponteiro" ligado
                (ok1 && ok2).then_some(SystemMouseInfo { enhance_precision: acc[2] != 0, speed })
            }
        }
        fn backend(&self) -> &'static str {
            "windows-raw-input/WM_INPUT"
        }
    }

    struct Ctx {
        tx: Sender<RawSample>,
        stats: Arc<CaptureStats>,
        buf: Vec<u64>,
    }

    thread_local! {
        static CTX: std::cell::RefCell<Option<Ctx>> = const { std::cell::RefCell::new(None) };
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    /// Thread dedicada: janela message-only, RIDEV_INPUTSINK, um WM_INPUT por evento.
    /// O timestamp é tirado no momento da leitura, independente do frame rate.
    ///
    /// Usa WM_INPUT + GetRawInputData em vez de GetRawInputBuffer: numa thread que só faz
    /// isso, a fila não acumula nem a 8 kHz, e o caminho é o mais bem documentado.
    /// Se aparecer perda de eventos em teste real, trocar para leitura em lote.
    unsafe fn run(tx: Sender<RawSample>, stats: Arc<CaptureStats>, ready: Sender<Result<(), String>>) {
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
        let class = wide("RSpecAimRawInput");
        let hinst = GetModuleHandleW(std::ptr::null());
        let wc = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst,
            hIcon: std::ptr::null_mut(),
            hCursor: std::ptr::null_mut(),
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: class.as_ptr(),
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(0, class.as_ptr(), class.as_ptr(), 0, 0, 0, 0, 0, HWND_MESSAGE, std::ptr::null_mut(), hinst, std::ptr::null());
        if hwnd.is_null() {
            let _ = ready.send(Err("CreateWindowExW falhou".into()));
            return;
        }
        let dev = RAWINPUTDEVICE { usUsagePage: 0x01, usUsage: 0x02, dwFlags: RIDEV_INPUTSINK, hwndTarget: hwnd };
        if RegisterRawInputDevices(&dev, 1, std::mem::size_of::<RAWINPUTDEVICE>() as u32) == 0 {
            let _ = ready.send(Err("RegisterRawInputDevices falhou".into()));
            return;
        }
        CTX.with(|c| *c.borrow_mut() = Some(Ctx { tx, stats, buf: vec![0u64; 64] }));
        let _ = ready.send(Ok(()));
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            DispatchMessageW(&msg);
        }
    }

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        if msg == WM_INPUT {
            let t = rspec_core::now();
            CTX.with(|c| {
                if let Some(ctx) = c.borrow_mut().as_mut() {
                    read_one(ctx, lp as HRAWINPUT, t);
                }
            });
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }

    unsafe fn read_one(ctx: &mut Ctx, h: HRAWINPUT, t: f64) {
        let hdr = std::mem::size_of::<RAWINPUTHEADER>() as u32;
        let mut size = 0u32;
        GetRawInputData(h, RID_INPUT, std::ptr::null_mut(), &mut size, hdr);
        let words = (size as usize).div_ceil(8);
        if ctx.buf.len() < words {
            ctx.buf.resize(words, 0);
        }
        if GetRawInputData(h, RID_INPUT, ctx.buf.as_mut_ptr().cast(), &mut size, hdr) == u32::MAX {
            return;
        }
        let ri = &*(ctx.buf.as_ptr() as *const RAWINPUT);
        if ri.header.dwType != RIM_TYPEMOUSE {
            return;
        }
        let m = ri.data.mouse;
        ctx.stats.events.fetch_add(1, Ordering::Relaxed);
        if m.usFlags & MOUSE_MOVE_ABSOLUTE != 0 {
            ctx.stats.absolute.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let bf = m.Anonymous.Anonymous.usButtonFlags as u32;
        let mut b = 0u16;
        if bf & RI_MOUSE_LEFT_BUTTON_DOWN != 0 {
            b |= buttons::LEFT_DOWN;
        }
        if bf & RI_MOUSE_LEFT_BUTTON_UP != 0 {
            b |= buttons::LEFT_UP;
        }
        if m.lLastX == 0 && m.lLastY == 0 && b == 0 {
            return;
        }
        let _ = ctx.tx.send(RawSample {
            t,
            dx: m.lLastX,
            dy: m.lLastY,
            device: ri.header.hDevice as u64,
            buttons: b,
            batched: false,
        });
    }
}

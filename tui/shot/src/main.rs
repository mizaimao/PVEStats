//! Saves what a wlroots compositor (the dashboard kiosk's cage) shows on its
//! first output as a PNG, pointer included, through the screencopy
//! protocol, and prints the seat's capabilities: a pointer among them means
//! the compositor would draw one. Run inside the session, with
//! WAYLAND_DISPLAY and XDG_RUNTIME_DIR set. `tui/screenshot` does all that.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::os::fd::AsFd;

use wayland_client::protocol::{wl_buffer, wl_output, wl_registry, wl_seat, wl_shm, wl_shm_pool};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1 as frame, zwlr_screencopy_manager_v1 as manager,
};

#[derive(Default)]
struct State {
    seat: Option<wl_seat::WlSeat>,
    capabilities: Option<wl_seat::Capability>,
    output: Option<wl_output::WlOutput>,
    shm: Option<wl_shm::WlShm>,
    manager: Option<manager::ZwlrScreencopyManagerV1>,
    /// Format, width, height and stride of the first buffer offered.
    format: Option<(wl_shm::Format, u32, u32, u32)>,
    offers_done: bool,
    ready: bool,
    failed: bool,
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(s: &mut Self, r: &wl_registry::WlRegistry, e: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_registry::Event::Global { name, interface, version } = e {
            match interface.as_str() {
                "wl_seat" => s.seat = Some(r.bind(name, version.min(5), qh, ())),
                "wl_output" if s.output.is_none() => s.output = Some(r.bind(name, 1, qh, ())),
                "wl_shm" => s.shm = Some(r.bind(name, 1, qh, ())),
                "zwlr_screencopy_manager_v1" => s.manager = Some(r.bind(name, version.min(3), qh, ())),
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(s: &mut Self, _: &wl_seat::WlSeat, e: wl_seat::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(c) } = e {
            s.capabilities = Some(c);
        }
    }
}

impl Dispatch<frame::ZwlrScreencopyFrameV1, ()> for State {
    fn event(s: &mut Self, _: &frame::ZwlrScreencopyFrameV1, e: frame::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match e {
            frame::Event::Buffer { format: WEnum::Value(f), width, height, stride } if s.format.is_none() => {
                s.format = Some((f, width, height, stride))
            }
            frame::Event::BufferDone => s.offers_done = true,
            frame::Event::Ready { .. } => s.ready = true,
            frame::Event::Failed => s.failed = true,
            _ => {}
        }
    }
}

macro_rules! no_events {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for State {
            fn event(_: &mut Self, _: &$t, _: <$t as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
no_events!(wl_output::WlOutput, wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_buffer::WlBuffer, manager::ZwlrScreencopyManagerV1);

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "screen.png".into());
    let conn = Connection::connect_to_env().expect("no Wayland session: set WAYLAND_DISPLAY and XDG_RUNTIME_DIR");
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    conn.display().get_registry(&qh, ());
    let mut s = State::default();
    queue.roundtrip(&mut s).unwrap();
    queue.roundtrip(&mut s).unwrap();
    let pointer = s.capabilities.is_some_and(|c| c.contains(wl_seat::Capability::Pointer));
    println!("seat: {:?}; pointer: {}", s.capabilities, if pointer { "yes" } else { "no" });

    // 1: with the pointer drawn in, as it is on screen.
    let shot = s.manager.as_ref().expect("no screencopy").capture_output(1, s.output.as_ref().expect("no output"), &qh, ());
    while s.format.is_none() || !s.offers_done {
        queue.blocking_dispatch(&mut s).unwrap();
        assert!(!s.failed, "capture refused");
    }
    let (format, w, h, stride) = s.format.unwrap();
    let path = format!("{out}.raw");
    let mut file = OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&path).unwrap();
    file.set_len((stride * h) as u64).unwrap();
    let pool = s.shm.as_ref().unwrap().create_pool(file.as_fd(), (stride * h) as i32, &qh, ());
    let buffer = pool.create_buffer(0, w as i32, h as i32, stride as i32, format, &qh, ());
    shot.copy(&buffer);
    while !s.ready && !s.failed {
        queue.blocking_dispatch(&mut s).unwrap();
    }
    assert!(!s.failed, "copy failed");

    let mut raw = Vec::new();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.read_to_end(&mut raw).unwrap();
    std::fs::remove_file(&path).ok();
    // XRGB/ARGB8888 sit in memory as B, G, R, X; XBGR/ABGR8888 as R, G, B, X.
    let bgr = matches!(format, wl_shm::Format::Xrgb8888 | wl_shm::Format::Argb8888);
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            let p = &raw[(y * stride + x * 4) as usize..][..4];
            rgb.extend_from_slice(&if bgr { [p[2], p[1], p[0]] } else { [p[0], p[1], p[2]] });
        }
    }
    let mut png = png::Encoder::new(File::create(&out).unwrap(), w, h);
    png.set_color(png::ColorType::Rgb);
    png.write_header().unwrap().write_image_data(&rgb).unwrap();
    println!("{w}x{h} saved to {out}");
}

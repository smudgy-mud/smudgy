//! A kitty graphics transmit from a server, end to end: through the session's socket and
//! decoder into the buffer as an inline object, measured in the terminal pane's grid, and
//! drawn through the process-global image store.
use super::*;
use iced::advanced::widget::Operation;
use smudgy_core::session::connection::{GraphicsDecoder, InboundCompression, TlsMode};
use smudgy_protocol::graphics::{
    CELL_FIXED_SCALE, DecodeRequest, DecodeResult, DecodedPixels, Placement, encode_payload,
};
use smudgy_ui_shared::split_terminal_pane::{
    GridReport, ScrolledLayout, TerminalViewHandle, split_terminal_pane,
};
use smudgy_ui_shared::terminal_buffer::TerminalBuffer;
use std::cell::{Cell, RefCell};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::rc::Rc;

/// Expands `f=24` pixels to RGBA on the calling thread.
struct StubDecoder;

impl GraphicsDecoder for StubDecoder {
    fn decode(&self, request: DecodeRequest, done: Box<dyn FnOnce(DecodeResult) + Send + 'static>) {
        let rgba: Vec<u8> = request
            .data
            .chunks_exact(3)
            .flat_map(|px| [px[0], px[1], px[2], 0xff])
            .collect();
        done(Ok(DecodedPixels {
            width: request.width,
            height: request.height,
            rgba: rgba.into(),
        }));
    }
}

/// The placements an operation walk of the pane finds, with their boxes.
struct Placements(Vec<(Arc<Placement>, Rectangle)>);

impl Operation for Placements {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }
    fn custom(
        &mut self,
        _id: Option<&iced::widget::Id>,
        bounds: Rectangle,
        state: &mut dyn std::any::Any,
    ) {
        if let Some(placement) = state.downcast_ref::<Arc<Placement>>() {
            self.0.push((placement.clone(), bounds));
        }
    }
}

#[tokio::test]
async fn a_transmitted_image_lands_inline_measures_in_the_grid_and_draws_from_the_store() {
    let _fixture_lock = SESSION_FIXTURE_LOCK.lock().await;
    let (_home, _root, params) = support::session_fixture("", &[]);
    let mut params = Arc::try_unwrap(params)
        .ok()
        .expect("the fixture's params are unshared");
    params.graphics_decoder = Some(Arc::new(StubDecoder));
    let mut events = Box::pin(spawn_with_package_provider(
        Arc::new(params),
        fixture_package_provider(),
    ));

    // The server: one transmit-and-display of a 2x1 RGB image, then DA1, and the
    // replies in that order; it holds the socket open until the test is done.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("address").port();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("accept");
        socket
            .set_read_timeout(Some(Duration::from_millis(200)))
            .expect("read timeout");
        let payload = encode_payload(&[1, 2, 3, 4, 5, 6]);
        socket
            .write_all(format!("\x1b_Ga=T,i=1,f=24,s=2,v=1;{payload}\x1b\\\x1b[c\r\n").as_bytes())
            .expect("send the image and DA1");
        let ok = b"\x1b_Gi=1;OK\x1b\\";
        let da1 = b"\x1b[?62;c";
        let mut received = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let position = |received: &[u8], needle: &[u8]| {
            received
                .windows(needle.len())
                .position(|window| window == needle)
        };
        loop {
            if let (Some(ok_at), Some(da1_at)) = (position(&received, ok), position(&received, da1))
            {
                assert!(
                    ok_at < da1_at,
                    "the transmit is answered before DA1: {received:?}"
                );
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no replies; received {received:?}"
            );
            let mut buf = [0_u8; 256];
            match socket.read(&mut buf) {
                Ok(0) => panic!("the client closed before replying"),
                Ok(n) => received.extend_from_slice(&buf[..n]),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(error) => panic!("read failed: {error}"),
            }
        }
        done_rx.recv_timeout(Duration::from_secs(60)).ok();
    });

    // Connect once the runtime is up; the line carrying the placement arrives as a
    // buffer append whose one object is the placement.
    let mut tx = None;
    let mut placed = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    while placed.is_none() {
        let event = tokio::time::timeout_at(deadline, events.next())
            .await
            .expect("timed out waiting for the placement")
            .expect("session closed");
        match event.event {
            SessionEvent::RuntimeReady(sender) => {
                sender
                    .send(RuntimeAction::Connect {
                        host: Arc::new("127.0.0.1".to_string()),
                        port,
                        wss_url: None,
                        send_on_connect: None,
                        send_on_connect_redactions: Arc::new(Vec::new()),
                        encoding: None,
                        compression: InboundCompression::NONE,
                        tls: TlsMode::Off,
                        graphics: smudgy_core::models::server::GraphicsMode::default(),
                        graphics_local_files: false,
                    })
                    .expect("connect");
                tx = Some(sender);
            }
            SessionEvent::UpdateBuffer(updates) => {
                for update in updates.iter() {
                    if let BufferUpdate::Append(line) = update
                        && line.objects.is_some()
                    {
                        placed = Some(line.clone());
                    }
                }
            }
            _ => {}
        }
    }
    let tx = tx.expect("the runtime came up");
    let line = placed.expect("a line with the placement");
    let objects = line.objects.as_ref().unwrap();
    assert_eq!(objects.len(), 1, "{objects:?}");
    let placement = objects[0]
        .resource
        .clone()
        .downcast::<Placement>()
        .expect("the object's resource is the placement");
    let (columns, rows) = (placement.geometry.columns, placement.geometry.rows);
    // 2x1 pixels in the fallback 9x18 cell: no pane has reported this session a grid.
    assert_eq!((columns, rows), (1, 1));
    assert_eq!(
        line.text,
        " ".repeat(usize::from(columns)),
        "placeholder columns"
    );
    assert_eq!(objects[0].range, 0..usize::from(columns));
    // The decode lands on the socket task between read batches; the server has its
    // `OK` by now or soon.
    for _ in 0..500 {
        if placement.image.pixels().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(placement.image.pixels().is_some(), "decoded");

    // Mount the line in a terminal pane with the native renderer: the placement's box is
    // its columns and rows in the pane's own grid, which the pane reports in fixed point.
    let store = smudgy_ui::image_store();
    let bytes_before = store.ready_bytes();
    let buffer = RefCell::new(TerminalBuffer::new());
    buffer.borrow_mut().push_line(line.clone());
    let grid: Rc<Cell<Option<GridReport>>> = Rc::new(Cell::new(None));
    let reporter = {
        let grid = grid.clone();
        Rc::new(move |report: GridReport| grid.set(Some(report))) as Rc<dyn Fn(GridReport)>
    };
    let resolver = {
        let store = store.clone();
        smudgy_ui_shared::inline_object::resolver(move |object, metrics| {
            smudgy_widgets::with_image_store_context(&store, || {
                smudgy_widgets::inline_element(object, metrics)
            })
        })
    };
    let mut renderer = native_renderer();
    let mut pane: iced::Element<
        '_,
        smudgy_widgets::WidgetMessage,
        smudgy_theme::Theme,
        iced::Renderer,
    > = split_terminal_pane(
        buffer.borrow(),
        TerminalViewHandle::default(),
        None,
        None,
        Some(reporter),
        None,
        ScrolledLayout::FullPane,
        Some(resolver),
    );
    let mut tree = Tree::new(&pane);
    let size = Size::new(640.0, 560.0);
    let node =
        pane.as_widget_mut()
            .layout(&mut tree, &renderer, &layout::Limits::new(Size::ZERO, size));
    let report = grid.get().expect("the pane reported its grid");
    let scale = f32::from(CELL_FIXED_SCALE);
    let advance = f32::from(report.cell_width) / scale;
    let line_height = f32::from(report.cell_height) / scale;
    assert!(advance > 1.0 && line_height > 1.0, "{report:?}");
    let mut found = Placements(Vec::new());
    pane.as_widget_mut()
        .operate(&mut tree, Layout::new(&node), &renderer, &mut found);
    let [(mounted, bounds)] = &found.0[..] else {
        panic!("one placement mounted, found {}", found.0.len());
    };
    assert!(Arc::ptr_eq(mounted, &placement));
    let expected = Size::new(f32::from(columns) * advance, f32::from(rows) * line_height);
    assert!(
        (bounds.width - expected.width).abs() < 0.01
            && (bounds.height - expected.height).abs() < 0.01,
        "the box is columns by rows in the pane's grid: {bounds:?}, expected {expected:?}"
    );

    // One draw mints the handle in the store, under the image's key, from its pixels.
    use iced::advanced::Renderer as _;
    let viewport = Rectangle::with_size(size);
    let theme = smudgy_theme::smudgy();
    let style = iced::advanced::renderer::Style {
        text_color: iced::theme::Base::base(&theme).text_color,
    };
    renderer.reset(viewport);
    pane.as_widget().draw(
        &tree,
        &mut renderer,
        &theme,
        &style,
        Layout::new(&node),
        mouse::Cursor::Unavailable,
        &viewport,
    );
    let key = format!("kitty:{}", placement.image.uid);
    let cell = store.peek(&key).expect("ensured on the first draw");
    assert!(
        matches!(
            &*cell.state(),
            smudgy_widgets::EntryState::Ready {
                width: 2,
                height: 1,
                ..
            }
        ),
        "{:?}",
        cell.state()
    );
    assert_eq!(store.ready_bytes(), bytes_before + 2 * 4);
    assert!(
        placement.image.has_ui_slot(),
        "the entry is tied to the image"
    );

    drop(done_tx);
    server.join().expect("server thread");
    drop(tx);
    drop(events);
    join_fixture_runtime().await;
}

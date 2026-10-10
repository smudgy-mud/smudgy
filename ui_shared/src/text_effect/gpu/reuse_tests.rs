use super::*;
use iced::{
    Font, Pixels, Size,
    advanced::text::{self as api, Paragraph as _},
};

fn input(content: &str) -> CaptureInput {
    let paragraph = text::Paragraph::with_text(api::Text {
        content,
        bounds: Size::new(1000.0, 40.0),
        size: Pixels(24.0),
        line_height: api::LineHeight::default(),
        font: Font::MONOSPACE,
        align_x: api::Alignment::Left,
        align_y: iced::alignment::Vertical::Top,
        shaping: api::Shaping::Advanced,
        wrapping: api::Wrapping::None,
    });
    CaptureInput {
        identity: Arc::default(),
        region: Rectangle::with_size(paragraph.min_bounds()),
        paragraph,
        capture_scale: 4.0,
        counters: Arc::default(),
    }
}

#[test]
#[ignore = "requires hardware GPU; run serialized with the inline effect GPU tests"]
fn shared_pixels_preserve_owners_accounting_active_pressure_and_renderer_teardown() {
    let (device, queue) = iced::futures::executor::block_on(async {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..Default::default()
        });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                force_fallback_adapter: false,
                ..Default::default()
            })
            .await
            .unwrap();
        adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .unwrap()
    });
    let before = global_stats().resident_bytes;
    let mut pipeline =
        <Pipeline as shader::Pipeline>::new(&device, &queue, wgpu::TextureFormat::Rgba8Unorm);
    let capture = |input: &CaptureInput, pipeline: &mut Pipeline| {
        super::capture(
            &device,
            &queue,
            input,
            input.capture_scale,
            capture::Placement {
                origin: iced::Point::ORIGIN,
                scale: 1.0,
            },
            &mut pipeline.swash,
            &pipeline.mipmaps,
            &mut pipeline.pixels,
        )
    };
    let a = input("A repeated inscription");
    let b = input("A repeated inscription");
    assert!(!Arc::ptr_eq(&a.identity, &b.identity));
    let weak_identity = Arc::downgrade(&a.identity);
    let weak_paragraph = a.paragraph.downgrade();
    let weak_counters = Arc::downgrade(&a.counters);
    let first = capture(&a, &mut pipeline);
    let second = capture(&b, &mut pipeline);
    assert!(Arc::ptr_eq(&first.pixels, &second.pixels));
    assert_eq!(a.counters.stats().uploads, 1);
    assert_eq!(b.counters.stats().uploads, 0);
    assert_eq!(b.counters.stats().cache_hits, 1);
    assert_eq!(
        a.counters.stats().resident_bytes,
        b.counters.stats().resident_bytes
    );
    let bytes = first.pixels.bytes;
    assert_eq!(
        global_stats().resident_bytes - before,
        bytes + 2 * std::mem::size_of::<GlyphTable>(),
        "shared pixels counted once"
    );
    drop(a);
    assert!(
        weak_identity.upgrade().is_some(),
        "cached occurrence retains its independent identity"
    );
    drop(first);
    assert!(
        weak_identity.upgrade().is_none(),
        "idle pixels retain no occurrence identity"
    );
    assert!(
        weak_paragraph.upgrade().is_none(),
        "reusable pixels do not retain a paragraph"
    );
    assert!(
        weak_counters.upgrade().is_none(),
        "reusable pixels do not retain host counters"
    );
    drop(second);
    assert_eq!(b.counters.stats().resident_bytes, 0);
    assert_eq!(
        global_stats().resident_bytes - before,
        bytes,
        "idle pixels remain reusable"
    );
    let replay = input("A repeated inscription");
    let replay_texture = capture(&replay, &mut pipeline);
    assert_eq!(replay.counters.stats().cache_hits, 1);
    assert_eq!(replay.counters.stats().uploads, 0);
    drop(replay_texture);
    let live: Vec<_> = (0..64)
        .map(|i| capture(&input(&format!("Active {i:03}")), &mut pipeline))
        .collect();
    for i in 0..75 {
        drop(capture(&input(&format!("Idle {i:03}")), &mut pipeline));
        pipeline.pixels.trim();
    }
    // Idle churn must not evict the pixels owned by the 64 live occurrences.
    for (i, texture) in live.iter().enumerate() {
        let fresh = input(&format!("Active {i:03}"));
        let other = capture(&fresh, &mut pipeline);
        assert!(Arc::ptr_eq(&texture.pixels, &other.pixels));
        assert_eq!(fresh.counters.stats().uploads, 0);
    }
    drop(live);
    pipeline.pixels.trim();
    assert!(global_stats().resident_bytes - before <= cache::IDLE_BYTES);
    let evicted = input("A repeated inscription");
    drop(capture(&evicted, &mut pipeline));
    assert_eq!(
        evicted.counters.stats().uploads,
        1,
        "old idle entry was evicted"
    );
    drop(pipeline);
    assert_eq!(
        global_stats().resident_bytes,
        before,
        "renderer teardown releases all images"
    );
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
}

use smudgy_cloud::{
    CachedCloudMapper, ExitArgs, ExitDirection, LabelArgs, Mapper, RoomNumber, RoomUpdates,
    ShapeArgs, ShapeType, mapper::RoomKey,
};
use std::{fs, sync::Arc};
use tokio::time::{Duration, sleep};
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("SMUDGY_LOG").is_err() {
        // This only needs to be wrapped with unsafe because it isn't thread-safe;
        // this is ok because we're only going to use this once, on the current thread
        unsafe {
            std::env::set_var("SMUDGY_LOG", "trace");
        }
    }
    pretty_env_logger::init_custom_env("SMUDGY_LOG");

    println!("🚀 Smudgy Map Shared Cache Demo");
    println!("=================================");

    // Initialize the cloud backend
    let cache_dir = std::env::temp_dir().join(format!("smudgy-map-demo-{}", Uuid::new_v4()));

    let backend = Arc::new(CachedCloudMapper::new_cloud(
        "https://api.smudgy.org".to_string(),
        "smudgy_dev_key".to_string(),
        cache_dir.clone(),
    ));

    // Create the shared cache
    let mapper = Mapper::new(backend, cache_dir.clone());

    // === SETUP PHASE ===
    println!("\n📋 Phase 1: Setting up test area...");

    // Create a test area
    let area_id = mapper.create_area("Demo Dungeon".to_string()).await?;

    println!("✅ Created area: {area_id}");

    // Set some area properties
    mapper.set_area_property(area_id, "theme".to_string(), "dark".to_string());
    mapper.set_area_property(area_id, "difficulty".to_string(), "hard".to_string());

    // Create some rooms
    println!("\n🏠 Creating rooms...");

    for i in 1..=500 {
        let room_updates = RoomUpdates {
            is_secret: None,
            external_id: None,
            title: Some(format!("Room {i}")),
            description: Some(format!("This is test room number {i}")),
            level: Some(0),
            x: Some((i as f32) * 10.0),
            y: Some(0.0),
            color: Some("#ffffff".to_string()),
        };

        let room_key = RoomKey {
            area_id,
            room_number: RoomNumber(i),
        };

        mapper.upsert_room(room_key.clone(), room_updates);
        mapper.set_room_property(room_key, "visited".to_string(), "false".to_string());
    }

    // Create some exits
    println!("🚪 Creating exits...");
    for i in 1..499 {
        let exit_args = ExitArgs {
            id: None,
            connection_id: None,
            is_secret: None,
            from_direction: ExitDirection::East,
            to_area_id: Some(area_id),
            to_room_number: Some(RoomNumber(i + 1)),
            to_direction: Some(ExitDirection::West),
            path: None,
            is_hidden: false,
            is_closed: false,
            is_locked: false,
            weight: 1.0,
            command: None,
        };

        mapper
            .create_exit(
                RoomKey {
                    area_id,
                    room_number: RoomNumber(i),
                },
                exit_args,
            )
            .await?;
    }

    // Create a label
    let label_args = LabelArgs {
        id: None,
        is_secret: None,
        level: 0,
        x: 25.0,
        y: -10.0,
        width: 100.0,
        height: 20.0,
        horizontal_alignment: smudgy_cloud::HorizontalAlignment::Center,
        vertical_alignment: smudgy_cloud::VerticalAlignment::Center,
        text: "Demo Area".to_string(),
        color: "#000000".to_string(),
        background_color: Some("#ffffff".to_string()),
        font_size: 14,
        font_weight: 400,
    };
    mapper.create_label(area_id, label_args).await?;

    // Create a shape
    let shape_args = ShapeArgs {
        id: None,
        is_secret: None,
        level: 0,
        x: 0.0,
        y: -20.0,
        width: 100.0,
        height: 60.0,
        background_color: Some("#f0f0f0".to_string()),
        stroke_color: Some("#333333".to_string()),
        shape_type: ShapeType::RoundedRectangle,
        border_radius: 5.0,
        stroke_width: Some(2.0),
    };
    mapper.create_shape(area_id, shape_args).await?;

    println!("✅ Setup complete!");

    // === MULTI-THREADED ACCESS DEMO ===
    println!("\n🧵 Phase 2: Multi-threaded access demo...");

    // Clone the mapper for different threads
    let ui_cache = mapper.clone();
    let js_cache = mapper.clone();

    // Simulate UI thread
    let ui_handle = tokio::spawn(async move {
        for i in 0..10 {
            println!("🖥️  UI Thread iteration {}", i + 1);

            // Read operations are instant
            let atlas = ui_cache.get_current_atlas();

            let areas = atlas.areas();
            println!("   📋 Found {} areas", areas.len());

            let area = atlas.get_area(&area_id).expect("Area not found");

            // Get area properties (instant read)
            if let Some(theme) = area.get_property("theme") {
                println!("   🎨 Area theme: {theme}");
            }
            if let Some(difficulty) = area.get_property("difficulty") {
                println!("   ⚔️  Area difficulty: {difficulty}");
            }

            sleep(Duration::from_millis(100)).await;
        }
    });

    // Simulate JavaScript thread with search operations
    let js_handle = tokio::spawn(async move {
        for i in 0..5 {
            println!("🔧 JS Thread iteration {}", i + 1);

            // Read operations are instant
            let atlas = js_cache.get_current_atlas();

            // Search operations
            let search_results = atlas.get_rooms_by_title("Room");
            println!("   🔍 Found {} rooms matching 'Room'", search_results.len());

            for (_area_id, room) in search_results.take(2) {
                println!(
                    "   🏠 Found room: {} at ({}, {})",
                    room.get_title(),
                    room.get_x(),
                    room.get_y()
                );
            }

            sleep(Duration::from_millis(200)).await;
        }
    });

    // Simulate concurrent cache access
    let cache_clone = mapper.clone();
    let stress_handle = tokio::spawn(async move {
        for i in 0..500 {
            let atlas = cache_clone.get_current_atlas();
            // These all return immediately - no blocking
            let _ = atlas.areas();
            let _ = atlas.get_rooms_by_title("test");
            let _ = atlas.get_rooms_by_description("room");

            if i % 10 == 0 {
                println!("   ⚡ Stress test iteration {i}");
            }
        }
    });

    // Wait for all threads to complete
    let _ = tokio::join!(ui_handle, js_handle, stress_handle);

    // === SYNC VERIFICATION ===
    println!("\n🔄 Phase 3: Sync verification...");

    // Wait for background sync to complete
    println!("⏳ Waiting for background sync...");
    match mapper.wait_for_sync_completion(60).await {
        Ok(true) => println!("✅ All sync operations completed successfully!"),
        Ok(false) => println!("⚠️  Timeout reached with pending operations"),
        Err(()) => println!("❌ Some sync operations failed"),
    }

    // Print final diagnostics
    println!("🔍 Sync stats: {:?}", mapper.get_sync_stats());

    println!("\n🎉 Demo completed successfully!");

    fs::remove_dir_all(cache_dir).ok();
    Ok(())
}

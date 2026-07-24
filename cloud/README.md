# Smudgy Cloud Crate

The `smudgy_cloud` crate is Smudgy's cloud-backed data + service-client layer. It holds the high-performance mapping domain (designed for lock-free multi-threaded access with eventual consistency) plus the cloud service clients (`CloudApiClient`, `PackageApiClient`) that share one credential source. It was renamed from `smudgy_map` when the package client landed.

## Architecture Philosophy

**Simple, Fast, Eventually Consistent**

This crate embraces simplicity over complex synchronization. Both UI and JavaScript threads share the same high-performance cache, with instant reads and fire-and-forget writes. Backend synchronization happens asynchronously without blocking either thread.

### Core Components

- **`MapperBackend`**: Core trait defining all map operations (CRUD for areas, rooms, exits, labels, shapes, properties)
- **`CloudMapper`**: HTTP client implementation connecting to the REST API; `CachedCloudMapper`, `LocalBackend`, and `CompositeBackend` are the other `MapperBackend` implementations
- **`Mapper`**: cheaply cloneable cache + background-sync front end providing lock-free access from any thread; backed by per-domain caches (`AreaCache`, `AtlasCache`, `ExitCache`, `RoomCache`)
- **`CloudApiClient` / `PackageApiClient`**: the cloud service clients — identity/social/sharing and `smudgy://` package sharing/discovery respectively — sharing the same `CredentialSource`
- **Comprehensive data structures**: `Area`, `Room`, `Exit`, `Label`, `Shape` with Arc-based sharing

### Key Design Principles

1. **Lock-Free Reads**: the whole atlas lives behind one `ArcSwap<AtlasCache>`; a read is an atomic snapshot load, so readers never block and never see a torn update
2. **Fire-and-Forget Writes**: updates land in cache via copy-on-write RCU (clone, rebuild, swap), then sync to backend async
3. **Single Cache Implementation**: Both UI and JavaScript threads use identical interface
4. **Arc-Based Sharing**: Zero-copy data sharing between threads
5. **Eventual Consistency**: Simplicity over complex rollback mechanisms

## Current Status

✅ **Completed**:
- Core data structures matching the backend API
- `MapperBackend` implementations: `CloudMapper` (HTTP), `CachedCloudMapper`, `LocalBackend`, `CompositeBackend`
- `Mapper` cache + background-sync front end with lock-free reads and fire-and-forget writes
- Per-domain caches with spatial (`rstar` R-tree) and property indices for fast queries
- `CloudApiClient` and `PackageApiClient` service clients
- Comprehensive error handling with `CloudResult<T>` / `CloudError`
- "areas" terminology aligned with the backend API

⏳ **Future**:
- Optimizations guided by the workspace benches (`../bench`: `mapper_scale`, `map_spatial` cover this crate)
- Cache eviction policies for memory management
- Advanced spatial indexing

## Usage

### Cached CloudMapper

```rust
use smudgy_cloud::{CachedCloudMapper, MapperBackend};
use std::path::PathBuf;

// Create a cached cloud mapper instance
let cache_dir = PathBuf::from("/path/to/map-cache");
let mapper = CachedCloudMapper::new_cloud(
    "https://api.smudgy.example.com".to_string(),
    "your-api-key".to_string(),
    cache_dir,
);

// List all areas
let areas = mapper.list_areas().await?;

// Get detailed area data  
let area_details = mapper.get_area(&area_id).await?;

// Create a new room
let room_updates = RoomUpdates {
    title: Some("Entrance Hall".to_string()),
    description: Some("A grand entrance hall".to_string()),
    level: Some(0),
    x: Some(0.0),
    y: Some(0.0),
    color: Some("#ffffff".to_string()),
    ..Default::default()
};
let room = mapper.update_room(&area_id, 1, room_updates).await?;
```

### Shared Cache

```rust
use smudgy_cloud::{CachedCloudMapper, Mapper};
use std::sync::Arc;
use std::path::PathBuf;

let cache_dir = PathBuf::from("/path/to/map-cache");
// Create backend and shared cache
let backend = Arc::new(CachedCloudMapper::new_cloud(base_url, api_key, cache_dir.clone()));
let cache = Mapper::new(backend, cache_dir.clone());

// Clone cache for use in different threads - zero cost!
let ui_cache = cache.clone();
let js_cache = cache.clone();

// UI Thread: Instant reads for rendering
let areas = ui_cache.get_all_areas();
let rooms_on_level = ui_cache.get_rooms_by_level(&area_id, 0);

// JavaScript Thread: Same interface, instant reads
let search_results = js_cache.search_rooms_by_title("Entrance Hall");

// Both threads: Fire-and-forget updates
ui_cache.update_room(area_id, 1, room_updates); // Returns immediately
js_cache.set_area_property(area_id, "name", "Dungeon"); // Returns immediately

// Backend sync happens automatically in background
```

### Multi-Thread Architecture

```rust
use std::sync::Arc;
use std::thread;
use std::path::PathBuf;

let cache_dir = PathBuf::from("/path/to/map-cache");
let cache = Arc::new(Mapper::new(backend, cache_dir));

// UI Thread
let ui_cache = cache.clone();
thread::spawn(move || {
    loop {
        // Instant access to area data for rendering
        let rooms = ui_cache.get_rooms_by_level(&area_id, current_level);
        render_rooms(rooms);
        std::thread::sleep(Duration::from_millis(16)); // 60 FPS
    }
});

// JavaScript Thread  
let js_cache = cache.clone();
thread::spawn(move || {
    // Script operations update cache immediately
    js_cache.set_room_property(area_id, room_id, "visited", "true");
    
    // Queries are instant - no waiting for synchronization
    let visited_rooms = js_cache.search_rooms_by_property("visited", "true");
    execute_script_with_data(visited_rooms);
});

// Background sync happens automatically - no thread management needed
```

## API Structure

The crate mirrors the existing HTTP API structure:

- **Areas**: Create, read, update, delete map areas (formerly "maps")
- **Area Properties**: Key-value metadata  
- **Rooms**: Individual locations with spatial data
- **Room Properties**: Custom metadata per room
- **Exits**: Connections between rooms (can span areas)
- **Labels**: Text annotations with positioning and alignment
- **Shapes**: Graphical elements (rectangles, rounded rectangles, etc.)

## Data Flow Design

```
UI Thread                  Shared Cache              JavaScript Thread
    |                  (ArcSwap snapshots)                  |
    | Instant Reads            |              Instant Reads |
    |<-------------------------|----------------------------->|
    |                          |                             |
    | Fire-and-Forget Updates  |   Fire-and-Forget Updates  |
    |------------------------->|<----------------------------|
    |                          |                             |
    |                     Background Sync                    |
    |                          |                             |
    |                          v                             |
    |                    Backend Storage                     |
    |                    (HTTP/SQLite)                       |
```

**Key Benefits**:
- No thread blocking - all operations return immediately
- No complex synchronization logic - `ArcSwap` RCU handles concurrency (readers keep their snapshot while a write swaps in the next one)
- No duplicate cache implementations - same code for all threads
- No MPSC channels - direct shared access
- Eventual consistency - simple and reliable

## Performance Characteristics

- **Read Operations**: one atomic `ArcSwap` snapshot load + O(1) hash lookup, no locks
- **Write Operations**: copy-on-write, **area-scoped** — the identification indices live in persistent maps (`imbl`), so a write edits only the touched area's entries (skipping rooms whose `Arc` survived the rewrite) and structurally shares every other area's, plus the touched area's connection/R-tree state, then swaps atomically. O(touched area), flat in total loaded rooms; the batch helpers (`upsert_rooms`, `with_areas_updated`) still amortize N changes into one pass. Only exclusion-axis changes (disable toggles, per-server scope) rebuild from scratch — rare, user-initiated. Backend sync is async and never blocks readers. Measured by `../bench` (`mapper_scale`, `gmcp_automap`).
- **Search Operations**: O(1) via the pre-built identification indices (title/description/exits; persistent-map probes cost a small constant over a flat table — identification stays µs-class, `find_room_by_external_id` ≈ 118 ns and scale-flat); spatial queries via `rstar` R-trees
- **Memory Sharing**: Zero-copy via Arc<> references
- **Thread Contention**: none on reads — writers pay the rebuild, readers keep their old snapshot until the swap

## Dependencies

- **`arc-swap`**: atomic snapshot pointer the whole cache sits behind (RCU writes)
- **`rstar`**: R-tree spatial indices for viewport/room queries
- **`reqwest`**: HTTP client for CloudMapper
- **`uuid`**: Area and entity identification  
- **`chrono`**: Timestamp handling
- **`serde`**: JSON serialization
- **`async-trait`**: Async trait support

## Development

### Testing
```bash
cargo test
```

### Linting
```bash
cargo clippy --all-features -- -D warnings
```

### Documentation
```bash
cargo doc --open
```

## Integration Points

This crate is designed for:

1. **UI Thread**: Instant area data access for 60fps rendering
2. **JavaScript Runtime**: Script-driven map operations without blocking
3. **Existing API**: Seamless integration with current HTTP backend
4. **Future Local Storage**: SQLite backend for offline usage

The simplified architecture eliminates complex channel systems while providing better performance and maintainability than the previous design.

## Recent Changes

**v0.1.0 - Backend Reconciliation Update**:
- Updated terminology from "maps" to "areas" to match backend API changes
- Added support for atlas hierarchy (areas can belong to atlases)
- Enhanced labels with width, height, and alignment properties
- Added weight and command fields to exits
- Updated shapes to use border_radius instead of radius
- Improved type safety with proper enum usage for directions and alignment
- All API endpoints now use `/areas` instead of `/maps`

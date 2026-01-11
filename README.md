# Foothold

Foothold is a Rust crate for managing session state in batch processing tasks. It provides utilities for tracking completed, failed, and skipped tasks, persisting results, and computing statistics for current and previous sessions. The crate is designed to be thread-safe and efficient for large-scale operations.

## Features
- Track completed, failed, and skipped tasks
- Persist session state to disk
- Compute statistics for current and previous sessions
- Thread-safe file writing
- Configurable sync options for disk operations

## Usage
Add foothold to your `Cargo.toml`:
```toml
[dependencies]
foothold = "0.1.0"
```

### Example

See crate documentation for a more examples.

```rust
use foothold::{Foothold, SyncConfig};
use std::path::Path;

// Define your task, success, and failure types
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
struct Task(String);
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
struct Success(String);
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
struct Failure(String);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let completed_path = Path::new("completed.jsonl");
    let failed_path = Path::new("failed.jsonl");
    let sync_config = SyncConfig::create(Some(10), Some(100));
    let mut foothold = Foothold::with_sync(completed_path, failed_path, 1000, sync_config)?;
    // ... use foothold to mark tasks and get stats ...
    Ok(())
}
```

## API Overview
- `Foothold<T, S, F>`: Main struct for session management
- `SyncConfig`: Configuration for disk sync operations
- `Stats`: Statistics for session progress

## Testing
Run tests with:
```sh
cargo test
```

## License
Licensed under either of
- Apache License, Version 2.0
- MIT license

at your option.

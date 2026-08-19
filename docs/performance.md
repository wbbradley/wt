# TUI performance checks

Cursor navigation and redraws share a 16 ms latency budget so the interface remains responsive at
60 Hz. The release-mode regression benchmark builds a synthetic GitHub model with 200 expanded
pull requests, checks, reviews, and unresolved feedback. The resulting tree contains at least 1,000
logical rows. It measures 1,000 alternating `j`/`k` cursor events together with their redraws and
fails when p95 reaches one frame.

Run it from the repository root:

```sh
cargo test --release --bin wt cursor_navigation_redraw_benchmark -- --ignored --nocapture
```

For a native macOS profile, build the release binary, launch `target/release/wt`, find its process
ID, exercise `j`/`k`, and sample it:

```sh
cargo build --release --bin wt
sample <pid> 10 -file /tmp/wt-cursor.sample.txt
```

The main-thread call graph must contain no `canonicalize`, `realpath`, or `__getattrlist` beneath
`wt::ui::render` or cursor-navigation handlers. Filesystem discovery during startup and explicit
repository refreshes is outside this render-path gate.

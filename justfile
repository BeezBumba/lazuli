mod ipl-hle "crates/ipl-hle"
mod dspint "crates/dspint"

export RUSTDOCFLAGS := "-Zunstable-options --show-type-layout --generate-link-to-definition --default-theme dark"

# Lists all recipes
list:
    @just --list

# Opens the documentation of the crates
doc:
    cargo doc --open

# Build the lazuli-web WASM package with wasm-pack.
# Install wasm-pack first: curl https://rustwasm.github.io/wasm-pack/installer/init.sh -sSf | sh
# Requires local/ipl-hle.dol to be present (run `just ipl-hle build` first).
web-build:
    cd crates/lazuli-web && wasm-pack build --target web --out-dir www/pkg
    cp local/ipl-hle.dol crates/lazuli-web/www/ipl-hle.dol

# Build lazuli-web and serve it on http://localhost:8080 with the COOP/COEP
# headers that enable SharedArrayBuffer (crossOriginIsolated = true).  These
# headers are required for the CPU Worker shared-memory audio ring buffer
# (Phase 3) and are also provided by the coi-serviceworker.js shim for
# static-host deployments that cannot set response headers.
web-serve: web-build
    cd crates/lazuli-web/www && python3 -c "
import http.server, ssl, sys, os

class COIHandler(http.server.SimpleHTTPRequestHandler):
    def end_headers(self):
        self.send_header('Cross-Origin-Opener-Policy',   'same-origin')
        self.send_header('Cross-Origin-Embedder-Policy', 'require-corp')
        super().end_headers()
    def log_message(self, fmt, *args):
        pass  # silence per-request noise

print('Serving http://localhost:8080 with COOP/COEP headers (crossOriginIsolated=true)')
http.server.HTTPServer(('', 8080), COIHandler).serve_forever()
"

# Build lazuli-web with WebAssembly atomic + bulk-memory features enabled.
# Required for future shared-WASM-linear-memory threading (Phase 4+).
# The current CPU Worker (Phase 2) and SAB audio (Phase 3) do NOT need this;
# they use JS-created SharedArrayBuffers which work with a normal WASM build.
#
# This recipe requires the nightly toolchain (already set by rust-toolchain.toml)
# and rebuilds the standard library with atomics support via -Z build-std.
web-build-atomics:
    cd crates/lazuli-web && \
        RUSTFLAGS="-C target-feature=+atomics,+bulk-memory,+mutable-globals" \
        wasm-pack build --target web --out-dir www/pkg -- \
            -Z build-std=panic_abort,std
    cp local/ipl-hle.dol crates/lazuli-web/www/ipl-hle.dol

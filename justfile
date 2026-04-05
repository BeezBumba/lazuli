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

# Build lazuli-web and serve it on http://localhost:8080
web-serve: web-build
    cd crates/lazuli-web/www && python3 -m http.server 8080

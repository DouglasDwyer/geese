/// Marks the crate as being compiled with unstable features enabled.
#[rustversion::nightly]
fn main() {
    println!("cargo:rustc-cfg=unstable");
}

/// Marks the crate as being compiled without unstable features.
#[rustversion::not(nightly)]
fn main() {}

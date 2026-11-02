//! Compile the small SEH/foreign-exception boundary with the existing pinned compiler helper.
fn main() {
    println!("cargo:rerun-if-changed=src/native.cpp");
    println!("cargo:rerun-if-changed=src/native.hpp");
    println!("cargo:rerun-if-changed=tests/native_driver.cpp");
    cc::Build::new()
        .cpp(true)
        .std("c++20")
        .flag_if_supported("/EHsc")
        .file("src/native.cpp")
        .file("tests/native_driver.cpp")
        .compile("ruler_boundary");
}

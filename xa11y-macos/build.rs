fn main() {
    #[cfg(target_os = "macos")]
    {
        cc::Build::new()
            .file("src/exception_safe.m")
            .flag("-fobjc-exceptions")
            .flag("-fmodules")
            .compile("exception_safe");

        println!("cargo:rustc-link-lib=framework=ApplicationServices");
        println!("cargo:rustc-link-lib=framework=Foundation");
    }
}

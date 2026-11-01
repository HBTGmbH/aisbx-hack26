use std::process::Command;

#[test]
fn the_version_flags_print_only_the_version() {
    for flag in ["--version", "-V"] {
        let out = Command::new(env!("CARGO_BIN_EXE_aisbx-ui"))
            .arg(flag)
            .output()
            .unwrap();
        assert!(out.status.success(), "{flag}");
        assert_eq!(
            String::from_utf8(out.stdout).unwrap(),
            format!("{}\n", aisbx_core::version()),
            "{flag}"
        );
    }
}

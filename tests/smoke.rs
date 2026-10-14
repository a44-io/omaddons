use assert_cmd::Command;

#[test]
fn binary_runs() {
    Command::cargo_bin("omaddons").unwrap().assert().success();
}

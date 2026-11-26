//! The one link a read-only grant may hold:
//! RubyInstaller's `ssl` junction into its own `lib`. Every other link, or
//! this one in a read/write grant or pointing elsewhere, is still refused.
#![cfg(windows)]

use std::path::Path;

use gardenwall_sandbox::Access;
use gardenwall_sandbox::windows::check_grant_tree;

fn junction(link: &Path, target: &Path) {
    let st = std::process::Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .unwrap();
    assert!(st.status.success(), "{st:?}");
}

/// A Ruby-like folder: `bin\ruby.exe` and `lib\ruby\3.4.0\etc\ssl`.
fn ruby(root: &Path) {
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::write(root.join("bin").join("ruby.exe"), b"").unwrap();
    std::fs::create_dir_all(root.join(r"lib\ruby\3.4.0\etc\ssl")).unwrap();
    std::fs::write(root.join(r"lib\ruby\3.4.0\etc\ssl\cert.pem"), b"x").unwrap();
}

#[test]
fn only_rubyinstallers_own_ssl_junction_is_allowed() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();

    let good = base.join("Ruby34-x64");
    ruby(&good);
    junction(&good.join("ssl"), &good.join(r"lib\ruby\3.4.0\etc\ssl"));
    check_grant_tree(&good, Access::ReadExecute).expect("the ssl junction is allowed");
    assert!(check_grant_tree(&good, Access::ReadWrite).is_err());

    let outside = base.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let escape = base.join("Ruby-escape");
    ruby(&escape);
    junction(&escape.join("ssl"), &outside);
    assert!(check_grant_tree(&escape, Access::ReadExecute).is_err());

    let elsewhere = base.join("Ruby-other");
    ruby(&elsewhere);
    junction(&elsewhere.join("ssl"), &elsewhere.join("bin"));
    assert!(check_grant_tree(&elsewhere, Access::ReadExecute).is_err());

    let named = base.join("Ruby-named");
    ruby(&named);
    junction(&named.join("certs"), &named.join(r"lib\ruby\3.4.0\etc\ssl"));
    assert!(check_grant_tree(&named, Access::ReadExecute).is_err());
    let not_ruby = base.join("tools");
    std::fs::create_dir_all(not_ruby.join(r"lib\ruby\3.4.0\etc\ssl")).unwrap();
    junction(
        &not_ruby.join("ssl"),
        &not_ruby.join(r"lib\ruby\3.4.0\etc\ssl"),
    );
    assert!(check_grant_tree(&not_ruby, Access::ReadExecute).is_err());

    let deep = base.join("Ruby-deep");
    ruby(&deep);
    junction(
        &deep.join("bin").join("ssl"),
        &deep.join(r"lib\ruby\3.4.0\etc\ssl"),
    );
    assert!(check_grant_tree(&deep, Access::ReadExecute).is_err());
}

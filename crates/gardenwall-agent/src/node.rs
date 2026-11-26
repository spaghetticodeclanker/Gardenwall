//! Node.js inside the Windows sandbox.
//!
//! Node's JavaScript `realpath` (used by `require`, ES module resolution,
//! npm and many tools) checks every folder from the drive root down with
//! `lstat`, and an AppContainer can't open `C:\` (EPERM), nor, on a default
//! Windows install, most folders above the granted ones. Every script failed
//! to start: npm, npx and `node script.js` alike.
//!
//! A small preload, passed inline in `NODE_OPTIONS` as a `data:` URL
//! (`--import`; a file would itself be loaded through realpath, and a
//! `data:` URL never touches the file system), answers only those calls:
//! when an `lstat` made by Node's own `realpathSync` fails with EPERM or
//! EACCES, it reports "a folder, not a link", so realpath walks on.
//! Everything else still gets the real error, links inside the granted
//! folders still resolve (pnpm-style layouts, for example), and the ES
//! module resolver, which keeps its own copy of `realpathSync`, is covered
//! too because both reach the same binding. The callback form `fs.realpath`
//! (Node's other JavaScript walk) uses the native call first and, if
//! that is refused as well, the answered synchronous walk.
//!
//! It grants nothing: it runs inside the same sandbox as the command. A
//! folder above the granted ones that really is a link is taken as a plain
//! folder; the path then isn't fully canonical, which only matters for
//! comparing paths. Needs Node.js 20.6 or newer (`--import` in
//! `NODE_OPTIONS`); Node 18 reached its end of life in April 2025.

#![cfg(windows)]

use std::ffi::OsString;

/// The preload, an ES module. It only changes how paths are resolved.
const PRELOAD: &str = r#"// Gardenwall, for this sandboxed session: Node's JavaScript realpath checks
// every folder from the drive root down, and the sandbox can't open the
// root or most folders above the project. When such a check, made by
// realpath itself, is refused, it is answered "a folder, not a link".
import fs from 'node:fs';
const binding = process.binding('fs');
const S_IFDIR = 0o040000;
const lstat = binding.lstat;
const calledByRealpath = () => /\brealpathSync \(node:fs:/.test(new Error().stack || '');
binding.lstat = function gardenwallLstat(path, useBigint, req, throwIfNoEntry) {
  if (req !== undefined) return lstat.apply(this, arguments);
  try {
    return lstat.apply(this, arguments);
  } catch (err) {
    if (err && (err.code === 'EPERM' || err.code === 'EACCES') && calledByRealpath()) {
      if (useBigint) {
        const out = new BigInt64Array(36);
        out[1] = BigInt(S_IFDIR);
        return out;
      }
      const out = new Float64Array(36);
      out[1] = S_IFDIR;
      return out;
    }
    throw err;
  }
};
const encodingOf = (o) =>
  typeof o === 'string' ? o : o && typeof o === 'object' && o.encoding ? { encoding: o.encoding } : undefined;
const jsAsync = fs.realpath;
const nativeAsync = jsAsync.native;
function realpath(p, options, callback) {
  if (typeof options === 'function') {
    callback = options;
    options = undefined;
  }
  nativeAsync(p, encodingOf(options), (err, resolved) => {
    if (!err) return callback(null, resolved);
    // The native call can be refused too (seen on Windows Server); the
    // synchronous walk, answered above, then resolves the path. Node's own
    // asynchronous walk would stop at the drive root.
    let out;
    try {
      out = fs.realpathSync(p, options);
    } catch (e) {
      return callback(e);
    }
    callback(null, out);
  });
}
realpath.native = nativeAsync;
fs.realpath = realpath;
"#;

/// Percent-encodes everything but unreserved URL characters, so the URL is
/// one `NODE_OPTIONS` word with no spaces or quotes.
fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The environment that loads the preload in every Node process the
/// sandbox starts.
pub fn env() -> Vec<(OsString, OsString)> {
    // Node resolves the entry script before it runs `--import` preloads, so
    // the entry alone keeps the path it was given; every module it loads,
    // dependencies included, resolves links as usual.
    vec![(
        "NODE_OPTIONS".into(),
        format!(
            "--preserve-symlinks-main --import data:text/javascript,{}",
            percent_encode(PRELOAD)
        )
        .into(),
    )]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_preload_is_one_word_without_quotes_or_spaces() {
        let (name, value) = env().remove(0);
        assert_eq!(name, "NODE_OPTIONS");
        let value = value.into_string().unwrap();
        let url = value
            .strip_prefix("--preserve-symlinks-main --import ")
            .unwrap();
        assert!(!url.contains([' ', '"', '\'', '\n', '\\']));
        assert!(url.starts_with("data:text/javascript,"));
        // Stays well under Windows' 32,767-character environment limit.
        assert!(value.len() < 8_000, "{}", value.len());
        assert_eq!(percent_encode("a b/é"), "a%20b%2F%C3%A9");
    }
}

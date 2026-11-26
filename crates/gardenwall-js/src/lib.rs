//! Runs one model-written JavaScript program (code mode, ADR-014 §2.1).
//!
//! The program is untrusted. This crate gives it a plain JavaScript engine
//! with no file, network, timer or module APIs. The only thing it can do
//! besides compute is ask its host to run a tool call ([`Transport::call`])
//! and wait for the answer. The shipped binary runs inside its own OS
//! sandbox and the host is the trusted core, which checks every call.

#![forbid(unsafe_code)]

use std::cell::RefCell;

use boa_engine::{Context, JsNativeError, JsResult, JsString, JsValue, NativeFunction, Source};
use serde::{Deserialize, Serialize};

/// Longest line either side accepts (a `write` call can carry a whole file).
pub const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;
/// Longest program text.
pub const MAX_CODE_BYTES: usize = 64 * 1024;
/// Most `console.log` text kept.
pub const MAX_LOG_BYTES: usize = 256 * 1024;
/// Most iterations of one loop.
const LOOP_LIMIT: u64 = 5_000_000;
/// Deepest call nesting.
const RECURSION_LIMIT: usize = 256;

/// The first line the core sends.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Start {
    /// The program text.
    pub run: String,
    /// The tool names offered as functions.
    pub tools: Vec<String>,
}

/// The core's answer to one call.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    pub result: String,
    pub error: bool,
}

/// A line from the helper to the core.
#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "lowercase")]
pub enum FromHelper {
    /// Run this tool and send back a [`Reply`].
    Call {
        name: String,
        args: serde_json::Value,
    },
    /// The program returned.
    Done { value: String, logs: String },
    /// The program threw, didn't parse or hit a limit.
    Failed { message: String, logs: String },
}

/// How the program reaches its host.
pub trait Transport {
    /// Run one tool call. `Ok((text, is_error))` is the tool's answer;
    /// `Err` means the host is gone and the program must end.
    fn call(&mut self, name: &str, args: serde_json::Value) -> Result<(String, bool), String>;
}

/// How a program ended.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Done { value: String, logs: String },
    Failed { message: String, logs: String },
}

struct Host {
    transport: RefCell<Box<dyn Transport>>,
    tools: Vec<String>,
    logs: RefCell<String>,
    /// `(failed, text)`, set once by the program's wrapper.
    finish: RefCell<Option<(bool, String)>>,
    /// Set when the host went away; the outcome is then a failure whatever
    /// the program does next.
    lost: RefCell<Option<String>>,
}

/// Defines each tool as a function and `console`, removes the three native
/// entry points (`__gw_*`) from the global scope, and evaluates to the
/// function that settles the program's promise. Nothing of the host is left
/// where the program can reach it, so it can't report its own outcome.
const PRELUDE: &str = r#"
(() => {
  const call = __gw_call, log = __gw_log, finish = __gw_finish;
  const show = (v) => {
    if (typeof v === "string") return v;
    if (v instanceof Error) return v.name + ": " + v.message;
    try { const s = JSON.stringify(v, null, 2); return s === undefined ? String(v) : s; }
    catch (_) { return String(v); }
  };
  // The listing tools give an array of lines, which is what programs loop
  // over. It still answers split() and trim(), and prints as lines, for
  // programs that treat it as the tool's text. A listing cut at the tool's
  // limit throws: a loop over part of a list would silently skip files.
  const hide = (o, k, v) => Object.defineProperty(o, k, { value: v, enumerable: false });
  const lines = (name, text) => {
    const empty = ["(empty directory)", "No files found matching pattern", "No matches found"];
    const m = /\n\n\[([^\n]*)\]\s*$/.exec(text);
    if (m && /limit reached/.test(m[1])) throw new Error(name + ": " + m[1]);
    const body = m ? text.slice(0, m.index) : text;
    const a = empty.includes(body.trim()) ? [] : body.split("\n").filter((l) => l !== "");
    hide(a, "split", () => a);
    hide(a, "trim", () => a.join("\n"));
    hide(a, "toString", () => a.join("\n"));
    return a;
  };
  const listing = ["ls", "find", "grep"];
  for (const name of __gw_tools) {
    globalThis[name] = (args) => {
      const r = JSON.parse(call(name, JSON.stringify(args === undefined ? {} : args)));
      if (r.error) throw new Error(r.result);
      return listing.includes(name) ? lines(name, r.result) : r.result;
    };
  }
  const print = (...a) => { log(a.map(show).join(" ")); };
  globalThis.console = { log: print, info: print, warn: print, error: print, debug: print };
  // The first frame in the program itself (the prelude has a file name).
  const fail = (e) => {
    let where = "";
    try {
      const m = /\(unknown at :(\d+):\d+\)/.exec(String(e && e.stack));
      if (m) where = " (line " + m[1] + ")";
    } catch (_) {}
    return show(e) + where;
  };
  delete globalThis.__gw_call;
  delete globalThis.__gw_log;
  delete globalThis.__gw_finish;
  delete globalThis.__gw_tools;
  return (p) => {
    Promise.resolve(p).then(
      (v) => finish(false, v === undefined ? "" : show(v)),
      (e) => finish(true, fail(e)),
    );
  };
})();
"#;

fn host(context: &Context) -> JsResult<&Host> {
    context
        .get_data::<Host>()
        .ok_or_else(|| JsNativeError::error().with_message("no host").into())
}

fn text(value: Option<&JsValue>) -> String {
    value
        .and_then(JsValue::as_string)
        .map(|s| s.to_std_string_lossy())
        .unwrap_or_default()
}

fn js_text(s: &str) -> JsValue {
    JsValue::from(JsString::from(s))
}

fn native_call(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let host = host(context)?;
    let name = text(args.first());
    let reply = |result: &str, error: bool| {
        Ok(js_text(
            &serde_json::json!({ "result": result, "error": error }).to_string(),
        ))
    };
    if let Some(why) = host.lost.borrow().as_ref() {
        return reply(why, true);
    }
    if !host.tools.contains(&name) {
        return reply(&format!("Unknown tool: {name}"), true);
    }
    let Ok(arguments) = serde_json::from_str::<serde_json::Value>(&text(args.get(1))) else {
        return reply("The arguments must be one object.", true);
    };
    if !arguments.is_object() {
        return reply("The arguments must be one object.", true);
    }
    match host.transport.borrow_mut().call(&name, arguments) {
        Ok((result, error)) => reply(&result, error),
        Err(why) => {
            *host.lost.borrow_mut() = Some(why.clone());
            reply(&why, true)
        }
    }
}

fn native_log(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let host = host(context)?;
    let mut logs = host.logs.borrow_mut();
    if logs.len() < MAX_LOG_BYTES {
        logs.push_str(&text(args.first()));
        logs.push('\n');
        if logs.len() >= MAX_LOG_BYTES {
            logs.push_str("[more console output was dropped]\n");
        }
    }
    Ok(JsValue::undefined())
}

fn native_finish(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let host = host(context)?;
    let failed = args.first().is_some_and(JsValue::to_boolean);
    let mut finish = host.finish.borrow_mut();
    if finish.is_none() {
        *finish = Some((failed, text(args.get(1))));
    }
    Ok(JsValue::undefined())
}

/// Run `code` to the end. `tools` become global functions that take one
/// arguments object and return the tool's text, or throw its error.
pub fn run(code: &str, tools: &[String], transport: Box<dyn Transport>) -> Outcome {
    let failed = |message: String, logs: String| Outcome::Failed { message, logs };
    if code.len() > MAX_CODE_BYTES {
        return failed("The program is too long.".into(), String::new());
    }
    let mut context = Context::default();
    let mut limits = context.runtime_limits();
    limits.set_loop_iteration_limit(LOOP_LIMIT);
    limits.set_recursion_limit(RECURSION_LIMIT);
    context.set_runtime_limits(limits);
    context.insert_data(Host {
        transport: RefCell::new(transport),
        tools: tools.to_vec(),
        logs: RefCell::new(String::new()),
        finish: RefCell::new(None),
        lost: RefCell::new(None),
    });

    let take_logs = |context: &Context| {
        context
            .get_data::<Host>()
            .map(|h| h.logs.borrow().clone())
            .unwrap_or_default()
    };
    let setup = (|| -> JsResult<boa_engine::JsObject> {
        for (name, function) in [
            (
                "__gw_call",
                native_call as fn(&JsValue, &[JsValue], &mut Context) -> _,
            ),
            ("__gw_log", native_log),
            ("__gw_finish", native_finish),
        ] {
            context.register_global_builtin_callable(
                JsString::from(name),
                2,
                NativeFunction::from_fn_ptr(function),
            )?;
        }
        let names = JsValue::from_json(&serde_json::json!(tools), &mut context)?;
        context.register_global_property(
            JsString::from("__gw_tools"),
            names,
            boa_engine::property::Attribute::all(),
        )?;
        let settle =
            context.eval(Source::from_bytes(PRELUDE).with_path(std::path::Path::new("prelude")))?;
        settle.as_callable().ok_or_else(|| {
            JsNativeError::error()
                .with_message("the prelude gave no settle function")
                .into()
        })
    })();
    let settle = match setup {
        Ok(settle) => settle,
        Err(e) => return failed(format!("The program could not start: {e}"), String::new()),
    };

    // The program is the body of an async function, so `return` and `await`
    // work. It starts on the wrapper's first line, so line numbers in errors
    // are the program's own. Its promise is handed to the prelude's settle
    // function from here, so the program never holds a way to report.
    let wrapped = format!("(async () => {{ {code}\n}})()");
    let ran = context
        .eval(Source::from_bytes(&wrapped))
        .and_then(|promise| settle.call(&JsValue::undefined(), &[promise], &mut context))
        .and_then(|_| context.run_jobs());
    let logs = take_logs(&context);
    let host = match context.get_data::<Host>() {
        Some(host) => host,
        None => return failed("The program lost its host.".into(), logs),
    };
    if let Some(why) = host.lost.borrow().clone() {
        return failed(why, logs);
    }
    if let Err(e) = ran {
        // An engine limit: the first line says which, the rest is a trace.
        let text = e.to_string();
        return failed(text.lines().next().unwrap_or_default().to_string(), logs);
    }
    match host.finish.borrow().clone() {
        Some((false, value)) => Outcome::Done { value, logs },
        Some((true, message)) => failed(message, logs),
        None => failed(
            "The program never finished (it is waiting on a promise that nothing resolves).".into(),
            logs,
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::*;

    type Seen = Rc<RefCell<Vec<(String, serde_json::Value)>>>;

    struct Fake {
        seen: Seen,
    }

    impl Transport for Fake {
        fn call(&mut self, name: &str, args: serde_json::Value) -> Result<(String, bool), String> {
            self.seen.borrow_mut().push((name.into(), args.clone()));
            match name {
                "read" => Ok((
                    format!("contents of {}", args["path"].as_str().unwrap_or("")),
                    false,
                )),
                "edit" => Ok(("No match for oldText".into(), true)),
                "ls" => Ok(("a.py\nsub/\nb.py".into(), false)),
                "find" => Ok(("No files found matching pattern".into(), false)),
                "grep" => Ok((
                    "a.py:1: x\n\n[100 matches limit reached. Use limit=200 for more, or refine pattern]"
                        .into(),
                    false,
                )),
                "gone" => Err("The app went away.".into()),
                _ => Ok(("ok".into(), false)),
            }
        }
    }

    fn go(code: &str) -> (Outcome, Vec<(String, serde_json::Value)>) {
        let seen: Seen = Rc::default();
        let tools = ["read", "edit", "write", "gone", "ls", "find", "grep"].map(String::from);
        let outcome = run(code, &tools, Box::new(Fake { seen: seen.clone() }));
        let calls = seen.borrow().clone();
        (outcome, calls)
    }

    fn done(code: &str) -> String {
        match go(code).0 {
            Outcome::Done { value, .. } => value,
            other => panic!("{other:?}"),
        }
    }

    fn failure(code: &str) -> String {
        match go(code).0 {
            Outcome::Failed { message, .. } => message,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn calls_reach_the_host_and_results_come_back() {
        let (outcome, calls) = go(r#"
            const out = [];
            for (const p of ["a.txt", "b.txt"]) out.push(read({ path: p }));
            return out.join("|");
        "#);
        assert_eq!(
            outcome,
            Outcome::Done {
                value: "contents of a.txt|contents of b.txt".into(),
                logs: String::new()
            }
        );
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[1],
            ("read".into(), serde_json::json!({ "path": "b.txt" }))
        );
    }

    #[test]
    fn objects_are_returned_as_json_and_logs_are_kept() {
        let (outcome, _) = go(r#"console.log("n", 3, { a: 1 }); return { ok: true };"#);
        let Outcome::Done { value, logs } = outcome else {
            panic!()
        };
        assert_eq!(value, "{\n  \"ok\": true\n}");
        assert_eq!(logs, "n 3 {\n  \"a\": 1\n}\n");
        assert_eq!(done("1 + 1;"), "");
    }

    #[test]
    fn listings_are_arrays_of_lines() {
        assert_eq!(
            done(
                "return ls({ path: '.' }).filter((n) => n.endsWith('.py')).map((n) => n.toUpperCase());"
            ),
            "[\n  \"A.PY\",\n  \"B.PY\"\n]"
        );
        // Programs that treat the result as the tool's text still work.
        assert_eq!(
            done(r#"return ls({}).split("\n").length + "|" + ls({}).trim() + "|" + ls({});"#),
            "3|a.py\nsub/\nb.py|a.py\nsub/\nb.py"
        );
        // Nothing found is an empty list, not a sentence to loop over.
        assert_eq!(
            done("let n = 0; for (const p of find({ pattern: '*.x' })) n++; return String(n);"),
            "0"
        );
        // A list cut at the tool's limit is an error, not a short list.
        assert!(
            failure("grep({ pattern: 'x' });")
                .starts_with("Error: grep: 100 matches limit reached")
        );
    }

    #[test]
    fn await_is_harmless() {
        assert_eq!(
            done(r#"const t = await read({ path: "x" }); return t;"#),
            "contents of x"
        );
    }

    #[test]
    fn a_tool_error_throws_and_can_be_caught() {
        assert_eq!(
            done(r#"try { edit({ path: "x" }); return "no"; } catch (e) { return e.message; }"#),
            "No match for oldText"
        );
        assert_eq!(
            failure("\n\nedit({ path: 'x' });"),
            "Error: No match for oldText (line 3)"
        );
    }

    #[test]
    fn unknown_tools_and_bad_arguments_are_refused_here() {
        assert!(failure("bash({ command: 'x' });").contains("bash"));
        let (outcome, calls) = go(r#"try { read("a.txt"); } catch (e) { return e.message; }"#);
        assert_eq!(
            outcome,
            Outcome::Done {
                value: "The arguments must be one object.".into(),
                logs: String::new()
            }
        );
        assert!(calls.is_empty());
    }

    #[test]
    fn syntax_errors_and_throws_fail_with_the_message() {
        assert!(failure("const = ;").contains("SyntaxError"));
        assert_eq!(
            failure(
                "
throw new TypeError('bad');"
            ),
            "TypeError: bad (line 2)"
        );
        assert_eq!(failure("throw 'plain';"), "plain");
    }

    #[test]
    fn endless_loops_and_recursion_stop() {
        assert!(!failure("while (true) {}").is_empty());
        assert!(!failure("const f = () => f(); f();").is_empty());
    }

    #[test]
    fn a_lost_host_ends_the_program_even_if_caught() {
        assert_eq!(
            failure(r#"try { gone({}); } catch (e) {} return "fine";"#),
            "The app went away."
        );
    }

    #[test]
    fn there_is_no_way_out_of_the_engine() {
        for name in [
            "require",
            "process",
            "fetch",
            "setTimeout",
            "Deno",
            "__gw_call",
            "__gw_log",
            "__gw_finish",
            "__gw_show",
            "__gw_fail",
            "__gw_tools",
        ] {
            assert_eq!(
                done(&format!("return typeof {name};")),
                "undefined",
                "{name}"
            );
        }
        assert!(failure("await import('fs');").len() > 1);
    }

    #[test]
    fn a_program_cannot_report_its_own_outcome() {
        // (Closing the wrapper early only picks which of its own values the
        // program returns: the model wrote both, and the core's count of
        // calls is added outside the program.) A program that throws can't turn that into success.
        assert!(
            failure("try { __gw_finish(false, 'fine'); } catch (_) {} throw new Error('real');")
                .starts_with("Error: real")
        );
    }

    #[test]
    fn a_program_that_is_too_long_is_refused() {
        let code = format!("return 1; //{}", "x".repeat(MAX_CODE_BYTES));
        assert_eq!(failure(&code), "The program is too long.");
    }
}

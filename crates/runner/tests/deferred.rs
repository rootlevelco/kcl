//! Deferred values: `@computed` attributes, the templates and conditions
//! built from them, instance naming, and known values.

use kcl_parser::ParseSession;
use kcl_runner::{ExecProgramArgs, exec_program};
use serde_json::{Value, json};
use std::sync::Arc;

const SCHEMAS: &str = r#"
schema File[_path: str = None]:
    _kind: str = "up.File"
    name?: str
    path?: str = _path
    content?: str
    when?: bool
    @computed
    sha256: str
    @computed
    json: any
    @computed(settable=True)
    mode: str

schema Log:
    _kind: str = "up.Log"
    name?: str
    message: str
"#;

fn run(name: &str, code: &str, known: Value) -> Result<Value, String> {
    let dir = std::env::temp_dir().join(format!("kcl-deferred-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.k");
    std::fs::write(&file, format!("{SCHEMAS}\n{code}")).unwrap();
    let args = ExecProgramArgs {
        work_dir: Some(dir.to_string_lossy().into_owned()),
        k_filename_list: vec![file.to_string_lossy().into_owned()],
        disable_none: true,
        show_hidden: true,
        deferred_kind_attr: "_kind".into(),
        deferred_name_attr: "name".into(),
        deferred_known: known.to_string(),
        ..Default::default()
    };
    let result =
        exec_program(Arc::new(ParseSession::default()), &args).map_err(|e| format!("{e:#}"))?;
    if !result.err_message.is_empty() {
        return Err(result.err_message);
    }
    Ok(serde_json::from_str(&result.json_result).unwrap())
}

fn ok(name: &str, code: &str) -> Value {
    run(name, code, json!({})).unwrap_or_else(|e| panic!("{e}"))
}

fn err(name: &str, code: &str) -> String {
    match run(name, code, json!({})) {
        Ok(v) => panic!("expected an error, got {v}"),
        Err(e) => e,
    }
}

#[test]
fn references_are_named_after_their_variable() {
    let out = ok(
        "refs",
        r#"
motd = File("/etc/motd")
copy = File("copy") { content = motd.sha256 }
"#,
    );
    assert_eq!(out["motd"]["name"], "motd");
    assert_eq!(out["motd"]["_kind"], "up.File");
    assert_eq!(
        out["motd"]["sha256"],
        json!({"_ref": "up.File/motd.sha256"})
    );
    assert_eq!(
        out["copy"]["content"],
        json!({"_ref": "up.File/motd.sha256"})
    );
}

#[test]
fn explicit_names_win_and_copies_keep_theirs() {
    let out = ok(
        "names",
        r#"
a = File("a") { name = "first" }
b = a
c = File("c") { content = b.sha256 }
items = [File("x"), Log { message = "hi" }]
"#,
    );
    assert_eq!(out["a"]["name"], "first");
    assert_eq!(out["b"]["name"], "first");
    assert_eq!(out["c"]["content"], json!({"_ref": "up.File/first.sha256"}));
    assert_eq!(out["items"][0]["name"], "items-0");
    assert_eq!(out["items"][1]["name"], "items-1");
}

#[test]
fn templates_and_paths() {
    let out = ok(
        "templates",
        r#"
v = File("v")
f = File("f") { content = "version=${v.json.version} of ${v.json['name']}\n" }
g = File("g") { content = "sum:" + v.sha256 }
"#,
    );
    assert_eq!(
        out["f"]["content"],
        json!({"_template": ["version=", {"_ref": "up.File/v.json.version"}, " of ", {"_ref": "up.File/v.json.name"}, "\n"]})
    );
    assert_eq!(
        out["g"]["content"],
        json!({"_template": ["sum:", {"_ref": "up.File/v.sha256"}]})
    );
}

#[test]
fn comparisons_and_logic_build_conditions() {
    let out = ok(
        "conds",
        r#"
v = File("v")
f = File("f") { when = v.sha256 == "abc" and not ("x" in v.json.tags) }
g = File("g") { when = True and v.json.n >= 3 }
h = File("h") { when = False and v.json.n >= 3 }
"#,
    );
    let r = |p: &str| json!({"_ref": format!("up.File/v.{p}")});
    assert_eq!(
        out["f"]["when"],
        json!({"_cond": "and", "args": [
            {"_cond": "eq", "args": [r("sha256"), "abc"]},
            {"_cond": "not", "args": [{"_cond": "contains", "args": [r("json.tags"), "x"]}]}
        ]})
    );
    assert_eq!(
        out["g"]["when"],
        json!({"_cond": "ge", "args": [r("json.n"), 3]})
    );
    assert_eq!(out["h"]["when"], json!(false));
}

#[test]
fn deferred_values_pass_type_checks() {
    let out = ok(
        "types",
        r#"
schema Port:
    number: int
    host: str

v = File("v")
p = Port { number = v.json.port, host = v.sha256 }
"#,
    );
    assert_eq!(out["p"]["number"], json!({"_ref": "up.File/v.json.port"}));
}

#[test]
fn known_values_answer_branches() {
    let code = r#"
v = File("v")
n = knownvalue(v.json.n) if isknown(v.json.n) else 0
many = "yes" if v.json.n > 2 else "no"
"#;
    let known = json!({"up.File/v.json.n": 5});
    let out = run("known", code, known).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(out["n"], 5);
    // A comparison stays a condition; branching on it forces its value.
    let e = run("unknown", code, json!({})).unwrap_err();
    assert!(
        e.contains("computed at runtime") || e.contains("isn't known"),
        "{e}"
    );
}

#[test]
fn misuse_is_an_error() {
    let e = err("set", "f = File(\"f\") { sha256 = \"x\" }");
    assert!(
        e.contains("sha256 is computed at runtime and can't be set"),
        "{e}"
    );
    let e = err("arith", "v = File(\"v\")\nn = v.json.n + 1");
    assert!(e.contains("computed at runtime"), "{e}");
    let e = err("branch", "v = File(\"v\")\nx = 1 if v.sha256 else 2");
    assert!(e.contains("isn't known yet"), "{e}");
    let e = err("static", "v = File(\"v\")\nn: int = v.sha256");
    assert!(e.contains("expected int, got str"), "{e}");
}

#[test]
fn wrapped_values_are_opaque_but_typed() {
    let out = ok(
        "wrap",
        r#"
secret = deferredwrap("_sensitive", "hunter2")
f = File("f") { content = secret }
g = File("g") { content = "pw=${secret}" }
"#,
    );
    assert_eq!(out["f"]["content"], json!({"_sensitive": "hunter2"}));
    assert_eq!(
        out["g"]["content"],
        json!({"_template": ["pw=", {"_sensitive": "hunter2"}]})
    );
}

#[test]
fn any_type_can_be_interpolated() {
    let out = ok(
        "interp",
        "v = File(\"v\")\nf = File(\"f\") { content = \"n=${v.json.n} size=${v.sha256}\" }\n",
    );
    assert_eq!(
        out["f"]["content"],
        json!({"_template": ["n=", {"_ref": "up.File/v.json.n"}, " size=", {"_ref": "up.File/v.sha256"}]})
    );
    // Typed computed values are checked statically, like any other value.
    let e = err("plus", "v = File(\"v\")\nx = 1 + v.sha256\n");
    assert!(e.contains("unsupported operand type(s) for +"), "{e}");
}

#[test]
fn settable_computed_attributes_are_inputs_when_set() {
    let out = ok(
        "settable",
        "a = File(\"a\") { mode = \"0644\" }\nb = File(\"b\")\nc = File(\"c\") { content = b.mode }\n",
    );
    assert_eq!(out["a"]["mode"], "0644");
    assert_eq!(out["b"]["mode"], json!({"_ref": "up.File/b.mode"}));
    assert_eq!(out["c"]["content"], json!({"_ref": "up.File/b.mode"}));
}

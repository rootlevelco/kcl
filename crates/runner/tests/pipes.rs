//! `|` on schemas with a `_pipe` lambda: pipelines, wherever they're written,
//! and written over several lines.

use kcl_ast::ast::ExternalPkg;
use kcl_parser::ParseSession;
use kcl_runner::{ExecProgramArgs, exec_program};
use serde_json::{Value, json};
use std::sync::Arc;

const SCHEMAS: &str = r#"
schema Stream:
    _kind: str = "up.Stream"
    name?: str
    in?: [str]
    source?: any
    through?: [any]
    _branch: bool = False
    @computed
    _self: any
    _pipe: (any) -> Stream = lambda next: any -> Stream {
        _through = (through or []) + [next]
        _from = source if _branch else _self
        Stream { source = _from, through = _through, _branch = True }
    }

schema Filter[_match: str = None]:
    match: str = _match

schema Aggregate[_window: str = None]:
    window: str = _window
    unique?: bool
"#;

const LOGS: &str = "logs = Stream { in = [\"a.log\"] }\n";

fn args(dir: &std::path::Path, files: &[(&str, &str)], own: bool) -> ExecProgramArgs {
    for (name, code) in files {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("{SCHEMAS}\n{code}")).unwrap();
    }
    ExecProgramArgs {
        work_dir: Some(dir.to_string_lossy().into_owned()),
        k_filename_list: vec![dir.join(files[0].0).to_string_lossy().into_owned()],
        disable_none: true,
        show_hidden: true,
        deferred_kind_attr: "_kind".into(),
        deferred_name_attr: "name".into(),
        deferred_known: "{}".into(),
        run_own_packages: own,
        ..Default::default()
    }
}

fn exec(name: &str, args: &ExecProgramArgs) -> Result<Value, String> {
    let result = exec_program(Arc::new(ParseSession::default()), args)
        .map_err(|e| format!("{name}: {e:#}"))?;
    if !result.err_message.is_empty() {
        return Err(result.err_message);
    }
    Ok(serde_json::from_str(&result.json_result).unwrap())
}

fn dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("kcl-pipes-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Runs `files`, the first of them the entry, every module running.
fn run_files(name: &str, files: &[(&str, &str)]) -> Result<Value, String> {
    let dir = dir(name);
    exec(name, &args(&dir, files, true))
}

fn run(name: &str, code: &str) -> Result<Value, String> {
    run_files(name, &[("main.k", code)])
}

/// `(name, what each stage matched on or windowed)` of the pipelines the
/// output holds apart from its variables.
fn pipes(out: &Value) -> Vec<(String, Vec<String>)> {
    out["_pipes"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|p| (p["name"].as_str().unwrap_or("").to_owned(), stages(p)))
        .collect()
}

fn stages(p: &Value) -> Vec<String> {
    let through = p["through"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    through
        .iter()
        .map(|s| {
            s.get("match")
                .or(s.get("window"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        })
        .collect()
}

fn branch(name: &str, stages: &[&str]) -> (String, Vec<String>) {
    (
        name.to_owned(),
        stages.iter().map(ToString::to_string).collect(),
    )
}

#[test]
fn a_pipeline_is_one_value_with_every_stage() {
    let out = run(
        "chain",
        &format!("{LOGS}whole = logs | Filter(\"e\") | Aggregate(\"5m\") {{ unique = True }} | Filter(\"x\")\n"),
    )
    .unwrap();
    assert_eq!(stages(&out["whole"]), ["e", "5m", "x"]);
    assert_eq!(out["whole"]["through"][1]["unique"], json!(true));
    assert_eq!(
        out["whole"]["source"],
        json!({"_ref": "up.Stream/logs._self"})
    );
    assert_eq!(out.get("_pipes"), None, "the variable has it");
}

#[test]
fn a_bare_pipeline_is_kept_and_named_after_where_it_starts() {
    let out = run(
        "bare",
        &format!(
            "{LOGS}logs | Filter(\"a\") | Aggregate(\"1m\")\nlogs | Filter(\"b\")\nafter = 1\n"
        ),
    )
    .unwrap();
    assert_eq!(
        pipes(&out),
        [branch("logs-1", &["a", "1m"]), branch("logs-2", &["b"])]
    );
    assert_eq!(
        out["after"],
        json!(1),
        "the output's variables are all there"
    );
    assert_eq!(out["logs"]["in"], json!(["a.log"]));
    assert_eq!(out.get("through"), None, "nothing merged into the output");
}

#[test]
fn branches_go_on_from_a_named_branch() {
    let out = run(
        "named",
        &format!("{LOGS}errors = logs | Filter(\"e\")\nerrors | Aggregate(\"1m\")\nerrors | Aggregate(\"5m\")\n"),
    )
    .unwrap();
    assert_eq!(stages(&out["errors"]), ["e"]);
    assert_eq!(
        pipes(&out),
        [
            branch("logs-1", &["e", "1m"]),
            branch("logs-2", &["e", "5m"])
        ]
    );
}

#[test]
fn a_pipeline_counts_wherever_it_is_written() {
    let code = format!(
        r#"{LOGS}
one = lambda s {{ logs | Filter(s) }}
one("lambda")
if True:
    logs | Filter("if")
if False:
    logs | Filter("not this")
[logs | Filter(s) for s in ["p", "q"]]
schema Holder:
    held = logs | Filter("schema")
holder = Holder {{}}
"#
    );
    let out = run("everywhere", &code).unwrap();
    let names: Vec<Vec<String>> = pipes(&out).into_iter().map(|p| p.1).collect();
    assert_eq!(names, [vec!["lambda"], vec!["if"], vec!["p"], vec!["q"]]);
    assert_eq!(stages(&out["holder"]["held"]), ["schema"]);
}

#[test]
fn a_module_runs_on_import_and_its_pipelines_count() {
    let module = format!("{LOGS}logs | Filter(\"bare\")\nkept = logs | Filter(\"named\")\n");
    let out = run_files(
        "module",
        &[("main.k", "import stacks.web\n"), ("stacks/web.k", &module)],
    )
    .unwrap();
    let got = pipes(&out);
    assert_eq!(got.len(), 3, "{got:?}");
    assert_eq!(got[0].1, Vec::<String>::new(), "where they start");
    assert_eq!(got[1].1, ["bare"]);
    assert_eq!(got[2].1, ["named"]);
    let first = &got[0].0;
    assert_eq!(got[1].0, format!("{first}-1"));
    assert!(
        first.starts_with("stacks") && got[2].0.starts_with("stacks"),
        "{got:?}"
    );
    assert_eq!(
        out["_pipes"][1]["source"],
        json!({"_ref": format!("up.Stream/{first}._self")})
    );
}

#[test]
fn a_module_imported_by_a_module_counts() {
    let out = run_files(
        "deep",
        &[
            ("main.k", "import stacks.web\n"),
            ("stacks/web.k", "import deep.more\n"),
            ("deep/more.k", &format!("{LOGS}logs | Filter(\"deep\")\n")),
        ],
    )
    .unwrap();
    assert_eq!(pipes(&out)[1].1, ["deep"]);
}

#[test]
fn modules_with_the_same_variable_keep_apart() {
    let module = format!("{LOGS}logs | Filter(\"x\")\n");
    let out = run_files(
        "apart",
        &[
            ("main.k", "import stacks.web\nimport stacks.api\n"),
            ("stacks/web.k", &module),
            ("stacks/api.k", &module),
        ],
    )
    .unwrap();
    let names: Vec<String> = pipes(&out).into_iter().map(|p| p.0).collect();
    let unique: std::collections::BTreeSet<&String> = names.iter().collect();
    assert_eq!(unique.len(), 4, "{names:?}");
}

#[test]
fn the_main_package_names_what_it_reads_from_a_module() {
    let module = format!("{LOGS}kept = logs | Filter(\"m\")\n");
    let out = run_files(
        "read",
        &[
            ("main.k", "import stacks.web\nmine = web.kept\n"),
            ("stacks/web.k", &module),
        ],
    )
    .unwrap();
    assert_eq!(stages(&out["mine"]), ["m"]);
    let got = pipes(&out);
    assert_eq!(
        got.len(),
        1,
        "only where it starts, which main doesn't have: {got:?}"
    );
    assert_eq!(
        out["mine"]["source"],
        json!({"_ref": format!("up.Stream/{}._self", got[0].0)})
    );
}

#[test]
fn without_the_option_a_module_nothing_refers_to_stays_out() {
    let dir = dir("lazy");
    let module = format!("{LOGS}logs | Filter(\"x\")\n");
    let files = [
        ("main.k", "import stacks.web\n"),
        ("stacks/web.k", module.as_str()),
    ];
    let out = exec("lazy", &args(&dir, &files, false)).unwrap();
    assert_eq!(out.get("_pipes"), None, "{out}");
}

#[test]
fn an_external_package_nothing_refers_to_stays_out() {
    let dir = dir("external");
    let pkg = dir.with_extension("vendored").join("vendored");
    let _ = std::fs::remove_dir_all(&pkg);
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(
        pkg.join("kcl.mod"),
        "[package]\nname = \"vendored\"\nedition = \"v0.11.0\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(
        pkg.join("main.k"),
        format!("{SCHEMAS}\n{LOGS}logs | Filter(\"x\")\n"),
    )
    .unwrap();
    let mut args = args(&dir, &[("main.k", "import vendored\n")], true);
    args.external_pkgs = vec![ExternalPkg {
        pkg_name: "vendored".into(),
        pkg_path: pkg.to_string_lossy().into_owned(),
    }];
    let out = exec("external", &args).unwrap();
    assert_eq!(out.get("_pipes"), None, "{out}");
}

#[test]
fn merging_is_what_it_was_without_a_pipe_lambda() {
    let code = r#"
schema Plain:
    x?: int
    y?: int
schema Base:
    labels: {str:str} = {a = "1"}
    labels |= {b = "2"}
merged = Plain { x = 1 } | Plain { y = 2 }
with_dict = Plain { x = 1 } | { y = 2 }
dicts = {a = 1} | {b = 2}
number = 6 | 1
base = Base {}
none = {a = 1} | None
"#;
    let out = run("merge", code).unwrap();
    assert_eq!(out["merged"], json!({"x": 1, "y": 2}));
    assert_eq!(out["with_dict"], json!({"x": 1, "y": 2}));
    assert_eq!(out["dicts"], json!({"a": 1, "b": 2}));
    assert_eq!(out["number"], json!(7));
    assert_eq!(out["base"]["labels"], json!({"a": "1", "b": "2"}));
    assert_eq!(out.get("_pipes"), None);
}

#[test]
fn unification_on_a_schema_with_a_pipe_lambda_still_merges() {
    let code = "logs: Stream { in = [\"a.log\"] }\nlogs: Stream { name = \"renamed\" }\n";
    let out = run("unify", code).unwrap();
    assert_eq!(out["logs"]["name"], json!("renamed"));
    assert_eq!(out["logs"]["in"], json!(["a.log"]));
    assert_eq!(out.get("_pipes"), None);
}

#[test]
fn what_a_pipe_makes_has_the_type_its_lambda_returns() {
    let code = r#"
schema Done:
    stages: int

schema Start:
    _pipe: (any) -> Done = lambda next: any -> Done { Done { stages = 1 } }

done = Start {} | Filter("x")
count: int = done.stages
"#;
    assert_eq!(run("types", code).unwrap()["count"], json!(1));
    let wrong = format!("{code}\nbad: str = done.stages\n");
    let err = run("types-wrong", &wrong).unwrap_err();
    assert!(err.contains("expected str, got int"), "{err}");
}

#[test]
fn a_stream_takes_only_schemas_and_configs() {
    let err = run("right", &format!("{LOGS}logs | \"oops\"\n")).unwrap_err();
    assert!(err.contains("unsupported operand type"), "{err}");
}

#[test]
fn a_method_built_by_a_method_gets_its_own_arguments() {
    let code = r#"
schema S:
    items: [str] = []
    add: (str) -> S = lambda x: str -> S {
        _i = items + [x]
        S { items = _i }
    }
r = S {}.add("a").add("b").add("c").items
"#;
    assert_eq!(run("scope", code).unwrap()["r"], json!(["a", "b", "c"]));
}

#[test]
fn a_pipeline_goes_on_over_indented_lines() {
    let code = format!(
        r#"{LOGS}
logs
    | Filter("a")  # a comment
    | Aggregate("5m") {{
        unique = True
    }}
    # | Filter("commented out")

    | Filter("b")
after = 1
assigned = logs
    | Filter("c")
    | Filter("d")
if True:
    logs
        | Filter("e")
        | Filter("f")
    in_if = 1
schema Holder:
    held = logs
        | Filter("g")
    more: int = 1
holder = Holder {{}}
call = lambda {{
    logs
        | Filter("h")
}}
called = call()
logs
    | Filter("last line")"#
    );
    let out = run("lines", &code).unwrap();
    assert_eq!(
        pipes(&out),
        [
            branch("logs-1", &["a", "5m", "b"]),
            branch("logs-2", &["e", "f"]),
            branch("logs-3", &["last line"]),
        ]
    );
    assert_eq!(out["through"], Value::Null);
    assert_eq!(stages(&out["assigned"]), ["c", "d"]);
    assert_eq!(stages(&out["holder"]["held"]), ["g"]);
    assert_eq!(out["holder"]["more"], json!(1));
    assert_eq!(stages(&out["called"]), ["h"]);
    assert_eq!(out["after"], json!(1));
    assert_eq!(out["in_if"], json!(1));
}

#[test]
fn any_union_may_go_on_over_indented_lines() {
    let out = run(
        "dicts",
        "d = {a = 1}\n    | {b = 2}\n    | {c = 3}\nafter = 1\n",
    )
    .unwrap();
    assert_eq!(out["d"], json!({"a": 1, "b": 2, "c": 3}));
    assert_eq!(out["after"], json!(1));
}

#[test]
fn an_indented_line_without_a_pipe_is_still_an_error() {
    let err = run("indent", &format!("{LOGS}logs\n    Filter(\"x\")\n")).unwrap_err();
    assert!(err.contains("unexpected token 'indent'"), "{err}");
}

#[test]
fn a_pipeline_used_before_it_is_defined_counts_once() {
    let code = format!(
        "{LOGS}first = second\nerrors | Aggregate(\"1m\")\nsecond = logs | Filter(\"s\")\nerrors = logs | Filter(\"e\")\n"
    );
    let out = run("forward", &code).unwrap();
    assert_eq!(stages(&out["second"]), ["s"]);
    assert_eq!(stages(&out["first"]), ["s"]);
    assert_eq!(pipes(&out), [branch("logs-1", &["e", "1m"])]);
}

#[test]
fn a_statement_after_a_pipeline_is_a_statement() {
    let code = format!(
        r#"{LOGS}
first = logs
    | Filter("a")
if True:
    second = 2
third = logs
    | Filter(
        "b"
    )
    | Aggregate("1m")
assert True
fourth = [
    1,
    2,
]
"#
    );
    let out = run("after", &code).unwrap();
    assert_eq!(stages(&out["first"]), ["a"]);
    assert_eq!(out["second"], json!(2));
    assert_eq!(stages(&out["third"]), ["b", "1m"]);
    assert_eq!(out["fourth"], json!([1, 2]));
}

#[test]
fn a_statement_level_with_the_pipes_is_an_error() {
    let err = run(
        "level",
        &format!("{LOGS}x = logs\n    | Filter(\"a\")\n    y = 1\n"),
    )
    .unwrap_err();
    assert!(err.contains("unexpected token 'indent'"), "{err}");
}

#[test]
fn a_line_starting_with_a_pipe_needs_its_indent() {
    let err = run("flush", &format!("{LOGS}logs\n| Filter(\"x\")\n")).unwrap_err();
    assert!(err.contains("InvalidSyntax"), "{err}");
}

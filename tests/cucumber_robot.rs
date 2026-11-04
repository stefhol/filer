//! End-to-end checks for the two acceptance-test formats:
//!
//! - `.feature` scenarios resolve to the step definitions that implement them
//!   (Cucumber-JVM annotations here; cucumber-js and pytest-bdd in the unit
//!   tests of `src/steps.rs`),
//! - `.robot` suites follow their `Resource`/`Variables` imports and resolve
//!   keyword calls into the resource files they reach.
//!
//! They run the real binary because the contract is about stdout and about the
//! paths on disk that Robot's relative settings resolve against.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("filer-bdd-it-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("features")).unwrap();
    fs::create_dir_all(dir.join("src/test/java/steps")).unwrap();
    fs::create_dir_all(dir.join("resources")).unwrap();
    fs::create_dir_all(dir.join("tests")).unwrap();

    fs::write(
        dir.join("features/login.feature"),
        "Feature: Login\n  Scenario: valid credentials\n    Given the site is up\n    When the user logs in as \"alice\"\n    Then the dashboard shows 3 widgets\n\n  Scenario Outline: many widgets\n    Given the cart has <count> items\n    Examples:\n      | count |\n      | 2     |\n      | 5     |\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/test/java/steps/LoginSteps.java"),
        "package steps;\n\npublic class LoginSteps {\n    @Given(\"^the site is up$\")\n    public void siteIsUp() {}\n\n    @When(\"the user logs in as {string}\")\n    public void logsIn(String name) {}\n\n    @Then(\"^the dashboard shows (\\\\d+) widgets$\")\n    public void shows(int count) {}\n\n    @Given(\"^the cart has (\\\\d+) items$\")\n    public void cartItems(int count) {}\n}\n",
    )
    .unwrap();
    fs::write(
        dir.join("resources/common.resource"),
        "*** Settings ***\nResource          nested.resource\n\n*** Keywords ***\nOpen Login Page\n    [Arguments]    ${url}\n    Log    opening ${url}\n",
    )
    .unwrap();
    fs::write(
        dir.join("resources/nested.resource"),
        "*** Settings ***\nVariables         ../vars.py\n\n*** Keywords ***\nType Credentials\n    Input Text    username    alice\n",
    )
    .unwrap();
    fs::write(dir.join("vars.py"), "URL = \"https://example.test\"\n").unwrap();
    fs::write(
        dir.join("tests/login.robot"),
        "*** Settings ***\nResource          ../resources/common.resource\nLibrary           SeleniumLibrary\n\n*** Test Cases ***\nValid Login\n    Open Login Page    ${URL}\n    type_credentials\n",
    )
    .unwrap();
    dir
}

fn run(root: &Path, args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_filer"))
        .arg("--root")
        .arg(root)
        .args(args)
        .env("XDG_CACHE_HOME", root.join(".cache"))
        .output()
        .expect("run filer");
    assert!(
        output.status.success(),
        "filer {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf-8 stdout")
}

fn object(text: &str) -> serde_json::Value {
    serde_json::from_str(text.trim()).expect("stdout must be one JSON object")
}

#[test]
fn index_lists_the_new_formats() {
    let root = project("index");
    let index = object(&run(&root, &["--json", "index"]));
    let languages: Vec<&str> = index["languages"]
        .as_array()
        .expect("languages")
        .iter()
        .map(|entry| entry["language"].as_str().expect("language"))
        .collect();
    assert!(languages.contains(&"gherkin"), "{languages:?}");
    assert!(languages.contains(&"robotframework"), "{languages:?}");

    // Scenarios, test cases and keywords are pickable rows.
    let search = run(&root, &["search", "--plain"]);
    assert!(search.contains("[function] valid credentials"), "{search}");
    assert!(search.contains("[function] many widgets"), "{search}");
    assert!(search.contains("[function] Valid Login"), "{search}");
    assert!(search.contains("[function] Open Login Page"), "{search}");
    // Function-granularity files have no whole-file row.
    assert!(
        !search.contains("[file] features/login.feature"),
        "{search}"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn feature_steps_resolve_to_step_definitions() {
    let root = project("steps");
    let deps = object(&run(
        &root,
        &["deps", "features/login.feature", "--unit", "--json"],
    ));
    let rows = deps["deps"].as_array().expect("deps");
    let mut units: Vec<(String, String)> = rows
        .iter()
        .map(|row| {
            (
                row["unit"].as_str().expect("unit").to_string(),
                row["reason"].as_str().expect("reason").to_string(),
            )
        })
        .collect();
    units.sort();
    assert!(
        units.contains(&(
            "src/test/java/steps/LoginSteps.java::LoginSteps.logsIn".to_string(),
            "step-definition".to_string()
        )),
        "{units:?}"
    );
    assert!(
        units.contains(&(
            "src/test/java/steps/LoginSteps.java::LoginSteps.siteIsUp".to_string(),
            "step-definition".to_string()
        )),
        "{units:?}"
    );

    // A single-scenario bundle pulls in the class shell plus the methods that
    // scenario's steps hit, and stubs the rest.
    let markdown = run(
        &root,
        &["bundle", "features/login.feature::valid credentials"],
    );
    assert!(markdown.contains("```gherkin"), "{markdown}");
    assert!(
        markdown.contains("src/test/java/steps/LoginSteps.java"),
        "{markdown}"
    );
    assert!(markdown.contains("void logsIn(String name)"), "{markdown}");
    // Methods the scenario does not reach stay signature stubs.
    assert!(
        !markdown.contains("public void cartItems(int count) {}"),
        "{markdown}"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn outline_steps_match_for_every_example_row() {
    let root = project("outline");
    // The outline expands to `the cart has 2 items` and `the cart has 5 items`;
    // the step definition is a regex, so both rows match the same method.
    let deps = object(&run(
        &root,
        &[
            "deps",
            "features/login.feature::many widgets",
            "--unit",
            "--json",
        ],
    ));
    let units: Vec<&str> = deps["deps"]
        .as_array()
        .expect("deps")
        .iter()
        .filter_map(|row| row["unit"].as_str())
        .collect();
    assert!(
        units.contains(&"src/test/java/steps/LoginSteps.java::LoginSteps.cartItems"),
        "{units:?}"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn robot_follows_resources_and_resolves_keywords() {
    let root = project("robot");
    let deps = object(&run(&root, &["deps", "tests/login.robot", "--json"]));

    let mut files: Vec<(String, String)> = deps["deps"]
        .as_array()
        .expect("deps")
        .iter()
        .map(|row| {
            (
                row["path"].as_str().expect("path").to_string(),
                row["reason"].as_str().expect("reason").to_string(),
            )
        })
        .collect();
    files.sort();
    assert!(
        files.contains(&(
            "resources/common.resource".to_string(),
            "path-import".to_string()
        )),
        "{files:?}"
    );
    // Nested resources are followed. The file is reached twice: as a resource
    // of `common.resource` and as the target of the suite's keyword call, so
    // the reason reported for it is whichever edge came first.
    assert!(
        files
            .iter()
            .any(|(path, _)| path == "resources/nested.resource"),
        "{files:?}"
    );
    assert!(
        files.contains(&("vars.py".to_string(), "path-import".to_string())),
        "{files:?}"
    );

    let edges = deps["edges"].as_array().expect("edges");
    assert!(
        edges.iter().any(|edge| {
            edge["from"] == "resources/common.resource"
                && edge["to"] == "resources/nested.resource"
                && edge["reason"] == "path-import"
        }),
        "{edges:?}"
    );

    // `type_credentials` is spelled differently from the keyword it calls.
    assert!(
        edges.iter().any(|edge| {
            edge["to"]
                .as_str()
                .is_some_and(|to| to.ends_with("nested.resource::Type Credentials"))
                && edge["reason"] == "call"
        }),
        "{edges:?}"
    );

    // A third-party `Library` is expected to stay unresolved.
    let diagnostics = deps["diagnostics"].as_array().expect("diagnostics");
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic["kind"] == "unresolved-import" && diagnostic["import"] == "SeleniumLibrary"
        }),
        "{diagnostics:?}"
    );

    let markdown = run(&root, &["bundle", "tests/login.robot", "--depth", "1"]);
    assert!(markdown.contains("```robotframework"), "{markdown}");
    assert!(markdown.contains("resources/common.resource"), "{markdown}");
    assert!(markdown.contains("Open Login Page"), "{markdown}");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn step_definition_callers_include_the_scenario() {
    let root = project("callers");
    let callers = run(
        &root,
        &[
            "callers",
            "src/test/java/steps/LoginSteps.java::LoginSteps.logsIn",
        ],
    );
    assert!(
        callers.contains("features/login.feature::valid credentials"),
        "{callers}"
    );

    let _ = fs::remove_dir_all(&root);
}

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn setup_vault() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("Notes/flashcard")).unwrap();
    dir
}

#[test]
fn create_dry_run_without_schema_is_unchanged() {
    let vault = setup_vault();

    Command::cargo_bin("vaultdb")
        .unwrap()
        .args([
            "--vault",
            vault.path().to_str().unwrap(),
            "--dry-run",
            "create",
            "Notes/flashcard",
            "--name",
            "hello",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("---\n---\n\n# hello\n"))
        .stdout(predicate::str::contains("db-table:").not());
}

#[test]
fn create_applies_schema_defaults_and_shows_in_dry_run() {
    let vault = setup_vault();
    std::fs::write(
        vault.path().join("vaultdb-schema.yaml"),
        r#"
collections:
  flashcard:
    folder: Notes/flashcard
    required: [db-table, card-type]
    fields:
      db-table:    { type: string,  default: flashcard }
      card-type:   { type: string,  enum: [basic, cloze] }
      state:       { type: string,  enum: [new, review], default: new }
      due:         { type: string,  default_expr: today }
      reps:        { type: integer, default: 0 }
      created_at:  { type: integer, default_expr: epoch }
"#,
    )
    .unwrap();

    Command::cargo_bin("vaultdb")
        .unwrap()
        .args([
            "--vault",
            vault.path().to_str().unwrap(),
            "--dry-run",
            "create",
            "Notes/flashcard",
            "--name",
            "hello",
            "--set",
            "card-type=cloze",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("db-table: flashcard"))
        .stdout(predicate::str::contains("card-type: cloze"))
        .stdout(predicate::str::contains("state: new"))
        .stdout(predicate::str::is_match(r"due: '20\d\d-\d\d-\d\d'").unwrap())
        .stdout(predicate::str::contains("reps: 0"))
        .stdout(predicate::str::is_match(r"created_at: \d+").unwrap());
}

#[test]
fn create_errors_when_required_missing_after_defaults() {
    let vault = setup_vault();
    std::fs::write(
        vault.path().join("vaultdb-schema.yaml"),
        r#"
collections:
  flashcard:
    folder: Notes/flashcard
    required: [db-table, card-type]
    fields:
      db-table:    { type: string,  default: flashcard }
      card-type:   { type: string,  enum: [basic, cloze] }
"#,
    )
    .unwrap();

    Command::cargo_bin("vaultdb")
        .unwrap()
        .args([
            "--vault",
            vault.path().to_str().unwrap(),
            "--dry-run",
            "create",
            "Notes/flashcard",
            "--name",
            "hello",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("schema-required field(s) missing"))
        .stderr(predicate::str::contains("card-type"));
}

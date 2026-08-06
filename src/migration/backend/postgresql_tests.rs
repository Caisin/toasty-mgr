use toasty::schema::db;

use super::{
    CREATE_LEDGER_SQL, LEDGER, postgres_auto_increment, postgres_storage_equivalent,
    split_postgresql_statements,
};

#[test]
fn manager_ledger_is_distinct_from_toasty_and_source_scoped() {
    assert_eq!(LEDGER, "__toasty_mgr_migrations");
    assert!(CREATE_LEDGER_SQL.contains("PRIMARY KEY (source_code, id)"));
    assert!(!CREATE_LEDGER_SQL.contains("REFERENCES"));
}

#[test]
fn normalizes_postgresql_identity_and_unsigned_integer_storage() {
    assert!(postgres_auto_increment("", "YES"));
    assert!(postgres_auto_increment("nextval('demo_id_seq')", "NO"));
    assert!(postgres_storage_equivalent(
        &db::Type::Integer(8),
        &db::Type::UnsignedInteger(8)
    ));
    assert!(!postgres_storage_equivalent(
        &db::Type::Integer(4),
        &db::Type::UnsignedInteger(8)
    ));
}

#[test]
fn splits_multiple_postgresql_commands() {
    let statements = split_postgresql_statements(
        "CREATE TABLE one (id BIGINT); CREATE INDEX one_id ON one (id);",
    )
    .unwrap();
    assert_eq!(
        statements,
        [
            "CREATE TABLE one (id BIGINT)",
            "CREATE INDEX one_id ON one (id)"
        ]
    );
}

#[test]
fn preserves_semicolons_in_postgresql_quotes() {
    let statements = split_postgresql_statements(
        "INSERT INTO \"semi;colon\" VALUES ('one;two', E'three\\\\;four', 'it''s;ok'); SELECT 1;",
    )
    .unwrap();
    assert_eq!(statements.len(), 2);
    assert!(statements[0].contains("\"semi;colon\""));
    assert!(statements[0].contains("'one;two'"));
    assert_eq!(statements[1], "SELECT 1");
}

#[test]
fn preserves_semicolons_in_postgresql_dollar_quotes() {
    let statements = split_postgresql_statements(
        "DO $$ BEGIN PERFORM 1; PERFORM 2; END $$; SELECT $body$one;two$body$;",
    )
    .unwrap();
    assert_eq!(
        statements,
        [
            "DO $$ BEGIN PERFORM 1; PERFORM 2; END $$",
            "SELECT $body$one;two$body$"
        ]
    );
}

#[test]
fn ignores_semicolons_in_postgresql_comments() {
    let statements = split_postgresql_statements(
        "-- leading ; comment\nCREATE TABLE one (id BIGINT); \
         /* outer ; /* nested ; */ done ; */ CREATE TABLE two (id BIGINT); \
         -- trailing ; comment",
    )
    .unwrap();
    assert_eq!(statements.len(), 2);
    assert!(statements[0].contains("CREATE TABLE one"));
    assert!(statements[1].contains("CREATE TABLE two"));
}

#[test]
fn toasty_breakpoints_remain_statement_boundaries() {
    let migration = db::Migration::new_sql(
        "CREATE TABLE one (id BIGINT)\n-- #[toasty::breakpoint]\nCREATE TABLE two (id BIGINT)"
            .to_owned(),
    );
    let statements = migration
        .statements()
        .into_iter()
        .flat_map(|sql| split_postgresql_statements(sql).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(statements.len(), 2);
    assert!(statements[0].contains("CREATE TABLE one"));
    assert!(statements[1].contains("CREATE TABLE two"));
}

#[test]
fn rejects_unterminated_postgresql_lexical_constructs() {
    for (sql, expected) in [
        ("SELECT 'open", "single-quoted string"),
        ("SELECT \"open", "quoted identifier"),
        ("SELECT $tag$open", "dollar-quoted string $tag$"),
        ("SELECT 1 /* open", "block comment"),
    ] {
        let error = split_postgresql_statements(sql).unwrap_err();
        assert!(
            error.to_string().contains(expected),
            "unexpected error for {sql:?}: {error:#}"
        );
    }
}

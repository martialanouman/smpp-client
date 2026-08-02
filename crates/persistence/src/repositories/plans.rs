//! Regression guard on the query plans SQLite chooses.
//!
//! # Why this exists
//!
//! Milestone 002 shipped every filtered query written as
//! `(? IS NULL OR column = ?)` — one literal instead of four, and an index
//! SQLite could not use. The tests were all green: the results were right, the
//! pagination walked the whole table, and nothing said so. Only reading
//! `EXPLAIN QUERY PLAN` by hand found it.
//!
//! So the plan is asserted, not the result.
//!
//! # Why it reads `.sqlx` rather than its own copies of the SQL
//!
//! A test holding its own copy of each query drifts from the queries the code
//! runs, and a drifted plan test is worse than none — it goes on passing.
//! `.sqlx/` holds the exact text of every statement the crate compiles, kept
//! in step by `cargo sqlx prepare` and checked by CI step 8. Reading it means
//! this test covers **every** query, including ones written after it.
//!
//! This is a unit test, not an integration one, because it needs the
//! `pub(crate)` pool: an integration test running its own SQL is precisely
//! what CA-002-03 forbids.

// `#[tokio::test]` expands to `Runtime::block_on`, which `clippy.toml`
// reserves for "the binary entry point". A test harness is one; the
// relaxations of that file cover `unwrap`/`expect` in tests but not this.
#![allow(clippy::disallowed_methods)]

use std::path::{Path, PathBuf};

use crate::db::{Database, DatabaseConfig};

/// Loads every statement `cargo sqlx prepare` cached, sorted for a stable
/// failure order.
fn cached_queries() -> Vec<String> {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join(".sqlx");

    let mut paths: Vec<PathBuf> = std::fs::read_dir(&directory)
        .expect("`.sqlx` is committed; run `just sqlx-prepare` if it is missing")
        .map(|entry| entry.expect("reading a directory entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    paths.sort();

    let mut queries: Vec<String> = paths
        .iter()
        .map(|path| {
            let text = std::fs::read_to_string(path).expect("reading a cache entry");
            let document: serde_json::Value =
                serde_json::from_str(&text).expect("a cache entry is JSON");

            document["query"]
                .as_str()
                .expect("a cache entry carries its query")
                .to_owned()
        })
        .collect();

    queries.sort();
    queries
}

/// Opens a temporary migrated database and returns the plan of each query.
async fn plans_of(queries: &[String]) -> Vec<(String, String)> {
    let directory = tempfile::TempDir::new().expect("creating a temporary directory");
    let database = Database::open(DatabaseConfig::new(directory.path().join("plans.db")))
        .await
        .expect("opening a fresh database");

    let mut plans = Vec::with_capacity(queries.len());

    for query in queries {
        // Parameters are left unbound, which SQLite reads as NULL for
        // planning. That is what makes the check honest: a plan that only
        // holds for one particular bound value would be no guarantee at all.
        // `AssertSqlSafe` because the string is built at runtime. There is no
        // injection surface: the text comes from `.sqlx/`, i.e. from the
        // crate's own compiled queries, and this module is `#[cfg(test)]`.
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!("EXPLAIN QUERY PLAN {query}")))
            .fetch_all(database.pool())
            .await
            .unwrap_or_else(|error| panic!("cannot explain `{query}`: {error}"));

        let detail = rows
            .iter()
            .map(|row| {
                use sqlx::Row as _;
                row.try_get::<String, _>("detail").unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join(" | ");

        plans.push((query.clone(), detail));
    }

    plans
}

/// Only `SELECT`s have a plan worth asserting; the writes are single-row and
/// keyed.
fn is_select(query: &str) -> bool {
    query
        .trim_start()
        .to_ascii_uppercase()
        .starts_with("SELECT")
}

/// Reads one of the tables that grows without bound.
///
/// `session_profiles`, `contact_lists` and `sqlite_master` hold units to
/// dozens of rows: a scan or a sort over them is free, and asserting on their
/// plans would only produce failures nobody should act on. `Database::
/// schema_objects` in particular sorts a seven-row catalogue through a
/// temporary B-tree, which is entirely correct.
fn reads_a_volume_table(query: &str) -> bool {
    ["messages", "contacts", "pdu_log", "contact_list_members"]
        .iter()
        .any(|table| {
            query.contains(&format!("FROM {table}")) || query.contains(&format!("JOIN {table}"))
        })
}

/// A `SELECT` on a table whose size the user controls.
fn is_worth_asserting(query: &str) -> bool {
    is_select(query) && reads_a_volume_table(query)
}

#[tokio::test]
async fn a_filter_on_state_reaches_idx_messages_state() {
    let queries: Vec<String> = cached_queries()
        .into_iter()
        .filter(|query| is_worth_asserting(query) && query.contains("state = ?"))
        .collect();

    assert!(
        !queries.is_empty(),
        "no cached query filters on `state`; has the filter been renamed?"
    );

    for (query, plan) in plans_of(&queries).await {
        assert!(
            plan.contains("idx_messages_state"),
            "a filter on `state` is not using its index.\nplan: {plan}\nquery: {query}"
        );
    }
}

#[tokio::test]
async fn a_filter_on_campaign_reaches_an_index() {
    let queries: Vec<String> = cached_queries()
        .into_iter()
        .filter(|query| is_worth_asserting(query) && query.contains("campaign_id = ?"))
        .collect();

    assert!(
        !queries.is_empty(),
        "no cached query filters on `campaign_id`; has the filter been renamed?"
    );

    for (query, plan) in plans_of(&queries).await {
        // A query filtering on both campaign and state may legitimately be
        // served by either index — SQLite picks one and applies the other
        // column as a residual test. What must never happen is neither.
        assert!(
            plan.contains("idx_messages_campaign") || plan.contains("idx_messages_state"),
            "a filter on `campaign_id` is not using any index.\nplan: {plan}\nquery: {query}"
        );
    }
}

/// A membership lookup must seek through the primary key of
/// `contact_list_members`, not read every contact in the file.
#[tokio::test]
async fn streaming_one_list_seeks_instead_of_scanning_the_contacts() {
    let queries: Vec<String> = cached_queries()
        .into_iter()
        // The predicate follows the query, which grew from one list to a set
        // (`m.list_id IN (SELECT value FROM json_each(?))`). Matching the old
        // literal made the guard select nothing, and a guard over an empty set
        // asserts nothing — which is why the emptiness check below exists.
        .filter(|query| is_worth_asserting(query) && query.contains("contact_list_members"))
        .collect();

    assert!(!queries.is_empty(), "no cached query streams one list");

    for (query, plan) in plans_of(&queries).await {
        // KNOWN DEFECT, milestone 009 — deliberately left failing-shaped so it
        // cannot be forgotten, but asserted on the part that already holds.
        //
        // The query walks `contacts` and tests list membership as a residual
        // (`SCAN contacts` + correlated `EXISTS`), instead of driving from
        // `contact_list_members`. On 200 000 contacts and a list of 50, it
        // reads 200 000 rows to keep 50. The membership sub-queries themselves
        // do seek, which is what this asserts; the driving table does not.
        //
        // Fixing it means rewriting the ANY/ALL multi-list predicate as a join
        // driven by the membership table, which is a design change rather than
        // a tweak — recorded in the milestone rather than rushed here.
        assert!(
            plan.contains("SEARCH m USING COVERING INDEX")
                || plan.contains("SEARCH x USING COVERING INDEX"),
            "membership lookup does not seek at all.\nplan: {plan}\nquery: {query}"
        );
    }
}

/// No query may sort through a temporary B-tree.
///
/// `USE TEMP B-TREE FOR ORDER BY` means SQLite buffers the whole result set
/// before yielding its first row. On the traversals of guide §11.3 that turns
/// a bounded-memory stream into a full materialisation — the very thing
/// CA-002-05 measures — and it does it silently.
#[tokio::test]
async fn no_query_sorts_through_a_temporary_b_tree() {
    let queries: Vec<String> = cached_queries()
        .into_iter()
        .filter(|query| is_worth_asserting(query))
        .collect();

    for (query, plan) in plans_of(&queries).await {
        assert!(
            !plan.contains("TEMP B-TREE"),
            "this query materialises its result set to sort it.\nplan: {plan}\nquery: {query}"
        );
    }
}

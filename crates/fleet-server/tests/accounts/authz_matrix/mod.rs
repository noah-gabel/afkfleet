//! The authorization matrix (Plan.md P13.1; ADR-0029): every route the
//! server registers, asked by every cast, and what each is answered.
//!
//! - **The rows** are [`rows::rows`]: each route at least once, and once
//!   more for every case that's answered differently (a mode on the
//!   allowlist and one off it, a session that stepped up and one that
//!   didn't). A route without a row fails
//!   [`the_matrix_has_a_row_for_every_route_and_no_other`], so a new route
//!   joins before it merges.
//! - **The columns** are [`world::Cast`]: nobody, a Member without a grant
//!   on the account the row is about, Members with View, Control and
//!   Manage on it, an Admin and the Owner. That account is a Member's.
//! - **A second block** asks the routes under a bot or an account again
//!   about an Admin's account, as a Member, another Admin and the Owner:
//!   whose account it is decides what an Admin may see, and no column of
//!   the first block shows that.
//! - **Every cell has a world of its own** ([`world::World`]): serve's app
//!   over a fresh database with the same people and things, so no cell's
//!   write, limit or lockout reaches another. A cell makes one request.
//! - **The table is a snapshot**, read cell by cell before it was accepted.
//!   What must hold whatever the table says is asserted on its own
//!   ([`broken_rules`], [`broken_rules_about_an_admins`]), so a careless
//!   accept can't let it through.

mod rows;
mod world;

use std::collections::BTreeSet;
use std::sync::Arc;

use fleet_core::authz::Role;
use fleet_server::config::RateLimitConfig;
use fleet_server::http::rate_limit::RateLimits;
use fleet_server::http::router::{api_routes, metrics_routes};
use fleet_server::http::routes::{Access, RouteEntry, Routes};
use fleet_testkit::system::ManualClock;
use futures_util::StreamExt as _;

use self::rows::{Row, Scenario, arrange, rows};
use self::world::{Cast, Cell, Shared, World};
use crate::support::service::{T0, at};

/// How many cells' worlds live at once.
const AT_ONCE: usize = 8;

/// The second block's casts.
const ABOUT_AN_ADMINS: [Cast; 3] = [Cast::Member, Cast::Admin, Cast::Owner];

/// A row with its route's access and its answers, one for each of its
/// block's casts, in their order.
struct Line {
    row: Row,
    access: Access,
    casts: &'static [Cast],
    cells: Vec<Cell>,
}

impl Line {
    /// The row's name in a message: its route and its case.
    fn name(&self) -> String {
        if self.row.case.is_empty() {
            route(&self.row)
        } else {
            format!("{} ({})", route(&self.row), self.row.case)
        }
    }

    /// What `cast` was answered.
    fn of(&self, cast: Cast) -> &Cell {
        let index = self
            .casts
            .iter()
            .position(|other| *other == cast)
            .expect("the cast is one of the block's");
        &self.cells[index]
    }

    /// Each user of the block with their answer: every cast but nobody.
    fn users(&self) -> impl Iterator<Item = (Cast, &Cell)> {
        self.casts
            .iter()
            .copied()
            .zip(&self.cells)
            .filter(|(cast, _)| *cast != Cast::Anonymous)
    }
}

/// A row's route, as the table writes it.
fn route(row: &Row) -> String {
    format!("{} {}", row.method, row.path)
}

/// The routes the second block asks again, and the ones a Member without
/// a grant must be answered 404 on: everything under a bot or an account.
fn under_a_bot_or_an_account(row: &Row) -> bool {
    ["/api/v1/bots/{id}", "/api/v1/accounts/{id}"]
        .iter()
        .any(|prefix| row.path.starts_with(prefix))
}

/// Every route the server registers: the API's and the metrics port's.
fn registry() -> Vec<RouteEntry> {
    let limits = RateLimits::new(
        &RateLimitConfig::default(),
        Arc::new(ManualClock::new(at(T0))),
    );
    let mut entries = api_routes(Routes::new(&limits)).entries().to_vec();
    entries.extend_from_slice(metrics_routes(Routes::new(&limits)).entries());
    entries
}

/// One cell: `row`'s case arranged in a world of its own, named `name`,
/// where `cast` asks and the holder has the role `holder`, and the one
/// request's answer.
async fn cell(shared: &Shared, name: String, row: &Row, cast: Cast, holder: Role) -> Cell {
    let mut world = World::start(shared, &name, cast, holder).await;
    let request = arrange(row, &mut world).await;
    let answer = world.answer(row.method.clone(), request).await;
    world.end().await;
    answer
}

/// A block of the matrix: `rows` asked by `casts` about the account of a
/// holder with the role `holder`, [`AT_ONCE`] cells at a time.
async fn block(shared: &Shared, rows: &[Row], casts: &'static [Cast], holder: Role) -> Vec<Line> {
    let registry = registry();
    let jobs = rows
        .iter()
        .enumerate()
        .flat_map(|(index, row)| casts.iter().map(move |cast| (index, row, *cast)));
    let cells: Vec<Cell> = futures_util::stream::iter(jobs)
        .map(|(index, row, cast)| {
            let name = format!("{holder:?}-{index}-{}", cast.label());
            cell(shared, name, row, cast, holder)
        })
        .buffered(AT_ONCE)
        .collect()
        .await;
    rows.iter()
        .zip(cells.chunks(casts.len()))
        .map(|(row, cells)| Line {
            row: row.clone(),
            access: registry
                .iter()
                .find(|entry| entry.method == row.method && entry.path == row.path)
                .map(|entry| entry.access)
                .expect("every row is a registered route"),
            casts,
            cells: cells.to_vec(),
        })
        .collect()
}

/// The whole matrix: every row about a Member's account, then the rows
/// under a bot or an account about an Admin's.
async fn matrix() -> (Vec<Line>, Vec<Line>) {
    let shared = Shared::new().await;
    let rows = rows();
    let again: Vec<Row> = rows
        .iter()
        .filter(|row| under_a_bot_or_an_account(row))
        .cloned()
        .collect();
    let first = block(&shared, &rows, &Cast::ALL, Role::Member).await;
    let second = block(&shared, &again, &ABOUT_AN_ADMINS, Role::Admin).await;
    (first, second)
}

/// The code a status usually comes with, which the table leaves out.
const fn usual_code(status: u16) -> &'static str {
    match status {
        401 => "unauthorized",
        403 => "forbidden",
        404 => "not_found",
        _ => "",
    }
}

/// A cell as the table writes it: the status, and the error's code unless
/// it's the status's usual one.
fn written(cell: &Cell) -> String {
    if cell.code.is_empty() || cell.code == usual_code(cell.status) {
        cell.status.to_string()
    } else {
        format!("{} {}", cell.status, cell.code)
    }
}

/// A block as text: a line per row, a column per cast.
fn grid(lines: &[Line]) -> String {
    let casts = lines.first().map_or(&[] as &[Cast], |line| line.casts);
    let mut grid: Vec<Vec<String>> = vec![
        ["route", "case"]
            .into_iter()
            .chain(casts.iter().map(|cast| cast.label()))
            .map(str::to_owned)
            .collect(),
    ];
    grid.extend(lines.iter().map(|line| {
        [route(&line.row), line.row.case.to_owned()]
            .into_iter()
            .chain(line.cells.iter().map(written))
            .collect()
    }));
    let widths: Vec<usize> = (0..grid[0].len())
        .map(|column| {
            grid.iter()
                .map(|cells| cells[column].chars().count())
                .max()
                .unwrap_or_default()
        })
        .collect();
    grid.iter()
        .map(|cells| {
            cells
                .iter()
                .zip(&widths)
                .map(|(cell, width)| format!("{cell:<width$}"))
                .collect::<Vec<_>>()
                .join("  ")
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The table: both blocks, each under what it's about.
fn table(first: &[Line], second: &[Line]) -> String {
    [
        "# Authorization matrix of the HTTP API",
        "Generated by fleet-server's `authz_matrix` test: every registered route, asked once by each cast in a world of its own.",
        "A cell is the status, and the error's code unless it's the usual one (401 unauthorized, 403 forbidden, 404 not_found).",
        "",
        "## Every route, about a Member's account",
        "The casts: nobody; a Member; Members with View, Control and Manage on that account; an Admin; the Owner.",
        "",
        &grid(first),
        "",
        "## The routes under a bot or an account, about an Admin's account",
        "The casts: a Member, another Admin and the Owner, none with a grant.",
        "",
        &grid(second),
        "",
    ]
    .join("\n")
}

/// Whether `cell` let its caller act: not one of the three refusals.
const fn acted(cell: &Cell) -> bool {
    !matches!(cell.status, 401 | 403 | 404)
}

/// What must hold of the first block whatever the snapshot says, for each
/// line that breaks it:
/// - every answer is an authorization's or the route's own, never a
///   request's shape (400, 405, 413, 415, 422), a limit (429) or a server
///   error; the one answer below 200 is the socket's 101;
/// - a public route answers every cast alike;
/// - any other route answers nobody with 401, and never a user with a
///   session on an authed route;
/// - a Member without a grant is answered 404 on every route under a bot
///   or an account, never 403: they may not learn that it exists;
/// - a higher grant may do what a lower one may, and the Owner what an
///   Admin may;
/// - each step-up route has a row where every user is asked to step up,
///   and one where none is.
fn broken_rules(lines: &[Line]) -> Vec<String> {
    let mut broken = Vec::new();
    for line in lines {
        let name = line.name();
        for (cast, cell) in line.casts.iter().zip(&line.cells) {
            let the_sockets = line.row.scenario == Scenario::Socket && cell.status == 101;
            if matches!(cell.status, 0..200 | 400 | 405 | 413 | 415 | 422 | 429 | 500..)
                && !the_sockets
            {
                broken.push(format!(
                    "{name}: {} was answered {}, which no authorization gives",
                    cast.label(),
                    written(cell)
                ));
            }
        }
        match line.access {
            Access::Public => {
                if line.cells.iter().any(|cell| cell != &line.cells[0]) {
                    broken.push(format!(
                        "{name}: a public route answers the casts differently"
                    ));
                }
            }
            Access::Authed | Access::AuthedStepUp | Access::Setup => {
                if line.of(Cast::Anonymous).status != 401 {
                    broken.push(format!(
                        "{name}: nobody was answered {}",
                        written(line.of(Cast::Anonymous))
                    ));
                }
                for (cast, cell) in line.users() {
                    if line.access != Access::Setup && cell.status == 401 {
                        broken.push(format!(
                            "{name}: {}'s session was answered 401",
                            cast.label()
                        ));
                    }
                }
            }
        }
        if under_a_bot_or_an_account(&line.row) && line.of(Cast::Member).status != 404 {
            broken.push(format!(
                "{name}: a Member without a grant was answered {}, not 404",
                written(line.of(Cast::Member))
            ));
        }
        for (lower, higher) in [
            (Cast::View, Cast::Control),
            (Cast::Control, Cast::Manage),
            (Cast::Admin, Cast::Owner),
        ] {
            if acted(line.of(lower)) && !acted(line.of(higher)) {
                broken.push(format!(
                    "{name}: {} may, but {} may not",
                    lower.label(),
                    higher.label()
                ));
            }
        }
    }
    let step_up_routes: BTreeSet<String> = lines
        .iter()
        .filter(|line| line.access == Access::AuthedStepUp)
        .map(|line| route(&line.row))
        .collect();
    for step_up in step_up_routes {
        let asks = |line: &Line| {
            line.users()
                .all(|(_, cell)| cell.status == 403 && cell.code == "step_up_required")
        };
        let goes_on = |line: &Line| {
            line.users()
                .all(|(_, cell)| cell.code != "step_up_required")
        };
        let of_route = || lines.iter().filter(|line| route(&line.row) == step_up);
        if !of_route().any(asks) {
            broken.push(format!(
                "{step_up}: no row where every user is asked to step up"
            ));
        }
        if !of_route().any(goes_on) {
            broken.push(format!(
                "{step_up}: no row where a stepped-up session goes on"
            ));
        }
    }
    broken
}

/// What must hold of the second block, for each line that breaks it: an
/// Admin's account is hidden from a Member and from another Admin on every
/// route (404, never 403), and the Owner is answered there as about a
/// Member's account.
fn broken_rules_about_an_admins(first: &[Line], second: &[Line]) -> Vec<String> {
    let mut broken = Vec::new();
    for line in second {
        let name = line.name();
        for cast in [Cast::Member, Cast::Admin] {
            if line.of(cast).status != 404 {
                broken.push(format!(
                    "{name}, an Admin's account: {} was answered {}, not 404",
                    cast.label(),
                    written(line.of(cast))
                ));
            }
        }
        let about_a_members = first
            .iter()
            .find(|other| other.name() == name)
            .map(|other| other.of(Cast::Owner));
        if about_a_members != Some(line.of(Cast::Owner)) {
            broken.push(format!(
                "{name}, an Admin's account: the Owner was answered {}, not as about a Member's",
                written(line.of(Cast::Owner))
            ));
        }
    }
    broken
}

/// The registry and the rows name the same routes: a new route fails here
/// until it has a row, and a row for a route that's gone fails too.
#[test]
fn the_matrix_has_a_row_for_every_route_and_no_other() {
    let registered: BTreeSet<String> = registry()
        .iter()
        .map(|entry| format!("{} {}", entry.method, entry.path))
        .collect();
    let rows = rows();
    let with_rows: BTreeSet<String> = rows.iter().map(route).collect();
    let cases: BTreeSet<(String, &str)> = rows.iter().map(|row| (route(row), row.case)).collect();

    assert_eq!(with_rows, registered);
    assert_eq!(cases.len(), rows.len(), "two rows are the same case");
}

/// The matrix: the rules that hold whatever it says, then the table.
#[tokio::test]
async fn every_route_answers_every_cast_as_the_matrix_says() {
    let (first, second) = matrix().await;

    let table = table(&first, &second);
    let mut broken = broken_rules(&first);
    broken.extend(broken_rules_about_an_admins(&first, &second));

    assert!(broken.is_empty(), "{}\n\n{table}", broken.join("\n"));
    insta::assert_snapshot!("authorization_matrix", table);
}

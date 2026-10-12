//! Keyset pages (Plan.md P7.6, P7.9, ADR-0016): the query every list takes,
//! [`PageQuery`], and the page size's bounds.
//!
//! A list answers its items newest first, and `next` when another page
//! follows: the `before` of that page, the last item's ID. Without `before`
//! a list starts at the newest. A page holds [`PAGE_DEFAULT`] items unless
//! `limit` says otherwise, at most [`PAGE_MAX`].
//!
//! The query isn't JSON, so it has no TypeScript type: it's read from the
//! URL's query string by the server and written by `fleet-client`. Its
//! `OpenAPI` parameters come from its own [`IntoParams`].

use garde::Validate;
use serde::{Deserialize, Serialize};
use utoipa::IntoParams;
use utoipa::openapi::Required;
use utoipa::openapi::path::{Parameter, ParameterBuilder, ParameterIn};

use crate::ids::id_schema;
use crate::int::integer_schema;

/// The most items one page holds.
pub const PAGE_MAX: u32 = 100;
/// How many items a page holds when the query names no `limit`.
pub const PAGE_DEFAULT: u32 = 50;

/// `?before=<id>&limit=<n>`: which page of a list to answer. `I` is the
/// list's ID type. A parameter that isn't one of these two, or one given
/// twice, is refused. A missing one reads as `None` without serde's
/// `default`, which would bound `I` by `Default`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct PageQuery<I> {
    /// Only items older than this one: the `next` of the page before.
    #[garde(skip)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<I>,
    /// How many items at most: 1 to [`PAGE_MAX`].
    #[garde(range(min = 1, max = PAGE_MAX))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

impl<I> PageQuery<I> {
    /// The page size: `limit`, or [`PAGE_DEFAULT`].
    #[must_use]
    pub fn limit_or_default(&self) -> u32 {
        self.limit.unwrap_or(PAGE_DEFAULT)
    }
}

impl<I> IntoParams for PageQuery<I> {
    fn into_params(_parameter_in_provider: impl Fn() -> Option<ParameterIn>) -> Vec<Parameter> {
        vec![
            ParameterBuilder::new()
                .name("before")
                .parameter_in(ParameterIn::Query)
                .required(Required::False)
                .description(Some(
                    "Only items older than this ID: the `next` of the page before.",
                ))
                .schema(Some(id_schema()))
                .build(),
            ParameterBuilder::new()
                .name("limit")
                .parameter_in(ParameterIn::Query)
                .required(Required::False)
                .description(Some("How many items at most; 50 by default."))
                .schema(Some(integer_schema::<1, PAGE_MAX>()))
                .build(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use fleet_core::id::UserId;
    use rstest::rstest;

    use super::*;

    fn problems(query: &PageQuery<UserId>) -> Vec<(String, String)> {
        query.validate().map_or_else(
            |report| {
                report
                    .iter()
                    .map(|(path, error)| (path.to_string(), error.message().to_owned()))
                    .collect()
            },
            |()| Vec::new(),
        )
    }

    fn query(limit: Option<u32>) -> PageQuery<UserId> {
        PageQuery {
            before: None,
            limit,
        }
    }

    #[rstest]
    #[case::none(None)]
    #[case::the_smallest(Some(1))]
    #[case::the_largest(Some(100))]
    fn a_limit_in_range_is_valid(#[case] limit: Option<u32>) {
        assert_eq!(problems(&query(limit)), []);
    }

    #[rstest]
    #[case::zero(0, "lower than 1")]
    #[case::one_too_many(101, "greater than 100")]
    fn a_limit_out_of_range_breaks_its_rule(#[case] limit: u32, #[case] message: &str) {
        assert_eq!(
            problems(&query(Some(limit))),
            [("limit".to_owned(), message.to_owned())]
        );
    }

    #[test]
    fn the_default_limit_is_fifty() {
        assert_eq!(query(None).limit_or_default(), 50);
        assert_eq!(query(Some(7)).limit_or_default(), 7);
    }

    #[test]
    fn the_query_names_two_parameters_in_the_query_string() {
        let params = serde_json::to_value(PageQuery::<UserId>::into_params(|| None)).unwrap();

        assert_eq!(params[0]["name"], "before");
        assert_eq!(params[0]["in"], "query");
        assert_eq!(params[0]["required"], false);
        assert_eq!(params[0]["schema"]["maxLength"], 36);
        assert_eq!(params[1]["name"], "limit");
        assert_eq!(params[1]["schema"]["minimum"], 1);
        assert_eq!(params[1]["schema"]["maximum"], 100);
        assert_eq!(params.as_array().unwrap().len(), 2);
    }
}

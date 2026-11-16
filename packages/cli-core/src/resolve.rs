//! Resolve human-friendly contest/problem references to the ids the API expects.

use crate::client::Client;
use anyhow::{Context, bail};

/// Resolve a contest reference to its numeric id; numeric is taken as-is, else match title.
pub fn contest_id(client: &Client, input: &str) -> anyhow::Result<String> {
    let q = input.trim();
    if q.is_empty() {
        bail!("Empty contest reference.");
    }
    if q.parse::<i64>().is_ok() {
        return Ok(q.to_string());
    }

    let contests = client
        .list_contests()
        .context("Failed to list contests to resolve the name")?
        .data;
    let lower = q.to_lowercase();

    let exact: Vec<_> = contests
        .iter()
        .filter(|c| c.title.to_lowercase() == lower)
        .collect();
    if exact.len() == 1 {
        return Ok(exact[0].id.to_string());
    }
    if exact.len() > 1 {
        bail!(
            "'{}' matches several contests by exact title. Use the numeric id ({}).",
            q,
            exact
                .iter()
                .map(|c| c.id.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let part: Vec<_> = contests
        .iter()
        .filter(|c| c.title.to_lowercase().contains(&lower))
        .collect();
    match part.as_slice() {
        [c] => Ok(c.id.to_string()),
        [] => bail!(
            "No contest matching '{}'. Run `broccoli contest list` to see contests.",
            q
        ),
        many => bail!(
            "'{}' matches several contests: {}. Be more specific or use the numeric id.",
            q,
            many.iter()
                .map(|c| format!("{} (#{})", c.title, c.id))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Resolve a problem reference within a contest to its numeric problem id.
pub fn problem_id(client: &Client, contest: &str, input: &str) -> anyhow::Result<String> {
    let q = input.trim();
    if q.is_empty() {
        bail!("Empty problem reference.");
    }

    let problems = client
        .list_contest_problems(contest)
        .context("Failed to list contest problems to resolve the reference")?;
    if problems.is_empty() {
        // nothing to match against; accept a raw numeric id
        if q.parse::<i64>().is_ok() {
            return Ok(q.to_string());
        }
        bail!("Contest {} has no problems to match '{}'.", contest, q);
    }

    Ok(match_problem(&problems, q)?.to_string())
}

/// Pure matcher for [`problem_id`], split out for unit testing without a server.
fn match_problem(
    problems: &[crate::api::contest::ContestProblemResponse],
    q: &str,
) -> anyhow::Result<i32> {
    let lower = q.to_lowercase();

    if let Some(p) = problems.iter().find(|p| p.label.to_lowercase() == lower) {
        return Ok(p.problem_id);
    }

    // real problem id first, else N-th displayed problem; NOT the raw 0-based, gappable `position`
    if let Ok(n) = q.parse::<i32>() {
        if let Some(p) = problems.iter().find(|p| p.problem_id == n) {
            return Ok(p.problem_id);
        }
        if n >= 1 && (n as usize) <= problems.len() {
            // sort defensively so the index is stable regardless of server order
            let mut ordered: Vec<&_> = problems.iter().collect();
            ordered.sort_by_key(|p| p.position);
            return Ok(ordered[(n - 1) as usize].problem_id);
        }
    }

    if let Some(p) = problems
        .iter()
        .find(|p| p.problem_title.to_lowercase() == lower)
    {
        return Ok(p.problem_id);
    }
    let part: Vec<_> = problems
        .iter()
        .filter(|p| p.problem_title.to_lowercase().contains(&lower))
        .collect();
    match part.as_slice() {
        [p] => Ok(p.problem_id),
        [] => bail!(
            "No problem matching '{}' in this contest. Available: {}.",
            q,
            available(problems)
        ),
        many => bail!(
            "'{}' matches several problems: {}. Use the label.",
            q,
            many.iter()
                .map(|p| format!("{} ({})", p.label, p.problem_title))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn available(problems: &[crate::api::contest::ContestProblemResponse]) -> String {
    problems
        .iter()
        .map(|p| format!("{} ({})", p.label, p.problem_title))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::match_problem;
    use crate::api::contest::ContestProblemResponse;

    fn p(problem_id: i32, label: &str, position: i32, title: &str) -> ContestProblemResponse {
        ContestProblemResponse {
            contest_id: 1,
            problem_id,
            label: label.into(),
            position,
            problem_title: title.into(),
        }
    }

    fn sample() -> Vec<ContestProblemResponse> {
        // ids unrelated to order; 0-based, gapped positions
        vec![
            p(10, "A", 0, "Two Sum"),
            p(11, "B", 2, "Subarrays"),
            p(12, "C", 5, "Graph Walk"),
        ]
    }

    #[test]
    fn label_case_insensitive() {
        assert_eq!(match_problem(&sample(), "A").unwrap(), 10);
        assert_eq!(match_problem(&sample(), "b").unwrap(), 11);
    }

    #[test]
    fn numeric_prefers_real_problem_id() {
        assert_eq!(match_problem(&sample(), "10").unwrap(), 10);
        assert_eq!(match_problem(&sample(), "12").unwrap(), 12);
    }

    #[test]
    fn numeric_falls_back_to_display_index() {
        // not problem ids, so N-th problem in position order, not the `position` value
        assert_eq!(match_problem(&sample(), "2").unwrap(), 11);
        assert_eq!(match_problem(&sample(), "3").unwrap(), 12);
        assert_eq!(match_problem(&sample(), "1").unwrap(), 10);
    }

    #[test]
    fn title_exact_and_substring() {
        assert_eq!(match_problem(&sample(), "Two Sum").unwrap(), 10);
        assert_eq!(match_problem(&sample(), "graph").unwrap(), 12);
    }

    #[test]
    fn unknown_errors() {
        assert!(match_problem(&sample(), "ZZZ").is_err());
        assert!(match_problem(&sample(), "99").is_err());
    }
}
